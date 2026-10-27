/// 生成多尺寸 (16x16, 24x24, 32x32, 48x48, 64x64) 32-bit BGRA 的“解开的锁 (🔓)” .ico 图标字节流。
/// 采用 4x4 超采样抗锯齿 (SSAA)，在深色和浅色 Windows 右键菜单、开始菜单及安装向导中均清晰锐利。
pub fn generate_unlocked_padlock_ico() -> Vec<u8> {
    let sizes: &[u32] = &[16, 24, 32, 48, 64];
    let mut images: Vec<Vec<u8>> = Vec::with_capacity(sizes.len());

    for &size in sizes {
        images.push(render_ico_bmp_slice(size));
    }

    let count = sizes.len() as u16;
    let header_size = 6 + (sizes.len() * 16);
    let total_size: usize = header_size + images.iter().map(|b| b.len()).sum::<usize>();
    let mut ico = Vec::with_capacity(total_size);

    // ICONDIR header
    ico.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Type = 1 (ICON)
    ico.extend_from_slice(&count.to_le_bytes()); // Count

    let mut offset = header_size as u32;
    for (&size, img) in sizes.iter().zip(images.iter()) {
        let dim = if size >= 256 { 0u8 } else { size as u8 };
        let bytes_in_res = img.len() as u32;
        ico.push(dim); // bWidth
        ico.push(dim); // bHeight
        ico.push(0); // bColorCount
        ico.push(0); // bReserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
        ico.extend_from_slice(&32u16.to_le_bytes()); // wBitCount (32-bit BGRA)
        ico.extend_from_slice(&bytes_in_res.to_le_bytes()); // dwBytesInRes
        ico.extend_from_slice(&offset.to_le_bytes()); // dwImageOffset
        offset += bytes_in_res;
    }

    for img in images {
        ico.extend_from_slice(&img);
    }

    ico
}

/// 渲染单个尺寸的 DIB (BITMAPINFOHEADER + 32-bit BGRA 像素 + 1-bit AND 掩码)
fn render_ico_bmp_slice(size: u32) -> Vec<u8> {
    let pixel_bytes = (size * size * 4) as usize;
    let mask_row_bytes = (size.div_ceil(32) * 4) as usize;
    let mask_bytes = mask_row_bytes * (size as usize);
    let mut out = Vec::with_capacity(40 + pixel_bytes + mask_bytes);

    // BITMAPINFOHEADER (40 bytes)
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight (XOR + AND)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    let mut and_mask = vec![0u8; mask_bytes];
    let inv = 1.0 / (size as f32);

    // DIB 像素从底行 (y = size - 1) 到顶行 (y = 0) 存储
    for row_bottom_up in 0..size {
        let py = (size - 1 - row_bottom_up) as f32;
        for px in 0..size {
            let pxf = px as f32;
            // 4x4 SSAA 超采样
            let mut r_sum = 0.0f32;
            let mut g_sum = 0.0f32;
            let mut b_sum = 0.0f32;
            let mut a_sum = 0.0f32;

            for sy in 0..4 {
                let v = (py + (sy as f32 + 0.5) * 0.25) * inv;
                for sx in 0..4 {
                    let u = (pxf + (sx as f32 + 0.5) * 0.25) * inv;
                    let (r, g, b, a) = sample_unlocked_padlock(u, v);
                    r_sum += r * a;
                    g_sum += g * a;
                    b_sum += b * a;
                    a_sum += a;
                }
            }

            let alpha = (a_sum / 16.0).clamp(0.0, 1.0);
            let (r, g, b, a_u8) = if alpha > 1e-4 {
                let r = (r_sum / a_sum).clamp(0.0, 255.0).round() as u8;
                let g = (g_sum / a_sum).clamp(0.0, 255.0).round() as u8;
                let b = (b_sum / a_sum).clamp(0.0, 255.0).round() as u8;
                let a = (alpha * 255.0).round() as u8;
                (r, g, b, a)
            } else {
                (0, 0, 0, 0)
            };

            out.push(b);
            out.push(g);
            out.push(r);
            out.push(a_u8);

            if a_u8 == 0 {
                let byte_idx = (row_bottom_up as usize) * mask_row_bytes + (px as usize) / 8;
                let bit_idx = 7 - ((px as usize) % 8);
                and_mask[byte_idx] |= 1 << bit_idx;
            }
        }
    }

    out.extend_from_slice(&and_mask);
    out
}

/// 在归一化坐标 (u, v) ∈ [0, 1]² 上采样“解开的锁 (🔓)”颜色 (R, G, B, Alpha)
/// u: 左(0.0) -> 右(1.0), v: 上(0.0) -> 下(1.0)
fn sample_unlocked_padlock(u: f32, v: f32) -> (f32, f32, f32, f32) {
    // 1. 锁身 (Golden Rounded Box):
    //    中心 (0.48, 0.70), 半宽 0.34, 半高 0.23, 圆角 0.08
    let body_dist = sd_rounded_box(u - 0.48, v - 0.70, 0.34, 0.23, 0.08);

    // 2. 解开的锁梁 (Open Shackle):
    //    向右上方倾斜抬起的 U 型金属环，左侧插入锁身左肩，右侧悬空张开，形成鲜明的“已解锁”缺口！
    let shackle_dist = sd_open_shackle(u, v);

    let mut out_r = 0.0f32;
    let mut out_g = 0.0f32;
    let mut out_b = 0.0f32;
    let mut out_a = 0.0f32;

    // 先绘制锁梁 (在锁身后方/上方)
    if shackle_dist < 0.0 {
        // 银白/青钢金属光泽渐变
        let t = ((u - 0.20) * 1.4).clamp(0.0, 1.0);
        let mut sr = 215.0 - t * 40.0;
        let mut sg = 228.0 - t * 35.0;
        let mut sb = 242.0 - t * 25.0;

        // 金属外边缘深色描边，保证在浅色菜单背景下也清晰可见
        if shackle_dist > -0.028 {
            sr = 75.0;
            sg = 95.0;
            sb = 118.0;
        } else if shackle_dist < -0.042 {
            // 金属高光芯
            sr = (sr + 28.0).min(255.0);
            sg = (sg + 28.0).min(255.0);
            sb = (sb + 22.0).min(255.0);
        }

        out_r = sr;
        out_g = sg;
        out_b = sb;
        out_a = 1.0;
    }

    // 再绘制锁身 (覆盖锁梁左下端接入处)
    if body_dist < 0.0 {
        // 琥珀金渐变 (上亮下深)
        let gy = ((v - 0.47) / 0.46).clamp(0.0, 1.0);
        let gx = ((u - 0.14) / 0.68).clamp(0.0, 1.0);
        let mut br = 255.0 - gy * 22.0 - gx * 8.0;
        let mut bg = 198.0 - gy * 52.0 - gx * 10.0;
        let mut bb = 28.0 - gy * 18.0;

        // 顶部内侧高光带
        if v > 0.49 && v < 0.54 && body_dist < -0.03 {
            br = (br + 25.0).min(255.0);
            bg = (bg + 35.0).min(255.0);
            bb = (bb + 45.0).min(255.0);
        }

        // 锁身深琥珀色外框
        if body_dist > -0.032 {
            br = 156.0;
            bg = 86.0;
            bb = 8.0;
        }

        // 3. 锁孔 (Keyhole): 上圆 + 下竖槽
        let kh_circle = ((u - 0.48).powi(2) + (v - 0.65).powi(2)).sqrt() - 0.058;
        let kh_slot = sd_rounded_box(u - 0.48, v - 0.74, 0.032, 0.075, 0.015);
        let kh_dist = kh_circle.min(kh_slot);

        if kh_dist < 0.0 {
            br = 58.0;
            bg = 34.0;
            bb = 10.0;
        } else if kh_dist < 0.018 {
            // 锁孔边缘微光
            br = 120.0;
            bg = 68.0;
            bb = 12.0;
        }

        out_r = br;
        out_g = bg;
        out_b = bb;
        out_a = 1.0;
    }

    (out_r, out_g, out_b, out_a)
}

/// 计算点 (x, y) 到圆角矩形 (半宽 hx, 半高 hy, 圆角 r) 的有符号距离
fn sd_rounded_box(x: f32, y: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = x.abs() - (hx - r);
    let qy = y.abs() - (hy - r);
    let out_dist = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let in_dist = qx.max(qy).min(0.0);
    out_dist + in_dist - r
}

/// 计算“解开的锁梁”有符号距离：
/// - 锁梁圆弧中心位于 (0.50, 0.27)，半径 R = 0.19，管径半宽 th = 0.058
/// - 左腿从 (0.31, 0.27) 向下延伸至 (0.31, 0.52) 插入锁身左肩
/// - 右腿从 (0.69, 0.27) 仅延伸至 (0.69, 0.30) 即截断悬空，与锁身顶部 (v = 0.47) 之间留出明显的“解开”缺口
fn sd_open_shackle(u: f32, v: f32) -> f32 {
    let cx = 0.50f32;
    let cy = 0.27f32;
    let radius = 0.19f32;
    let thickness = 0.056f32;

    // 顶部半圆弧 (v <= cy)
    let arc_center_dist = if v <= cy {
        let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
        (d - radius).abs()
    } else {
        f32::MAX
    };

    // 左竖腿: x = cx - radius = 0.31, y ∈ [0.27, 0.52]
    let left_x = cx - radius;
    let left_leg_dist = sd_segment(u, v, left_x, cy, left_x, 0.52);

    // 右短腿 (悬空解开端): x = cx + radius = 0.69, y ∈ [0.27, 0.30]
    let right_x = cx + radius;
    let right_leg_dist = sd_segment(u, v, right_x, cy, right_x, 0.30);

    arc_center_dist.min(left_leg_dist).min(right_leg_dist) - thickness
}

/// 点 (px, py) 到线段 (ax, ay)-(bx, by) 的距离
fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let pax = px - ax;
    let pay = py - ay;
    let bax = bx - ax;
    let bay = by - ay;
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    ((pax - bax * h).powi(2) + (pay - bay * h).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ico_has_valid_header_and_five_resolutions() {
        let ico = generate_unlocked_padlock_ico();
        assert!(ico.len() > 1024);
        // ICONDIR: 00 00 01 00 05 00
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 5, 0]);
        // 尺寸依次为 16, 24, 32, 48, 64
        let expected_sizes = [16u8, 24, 32, 48, 64];
        for (i, &sz) in expected_sizes.iter().enumerate() {
            let entry_offset = 6 + i * 16;
            assert_eq!(ico[entry_offset], sz);
            assert_eq!(ico[entry_offset + 1], sz);
        }
    }
}

$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot

cargo run --release --bin package -- $args
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

param(
    [switch]$Run
)

$ErrorActionPreference = "Stop"

Set-Location (Join-Path $PSScriptRoot "..")

cargo check --locked
cargo test --locked
cargo build --release --locked

Write-Host ""
Write-Host "Release binary: target\release\isaac_mod_manager.exe"

if ($Run) {
    cargo run
}

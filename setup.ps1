$ErrorActionPreference = 'Stop'

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "Rust is not installed. Install it first with: winget install --id Rustlang.Rustup -e, then reopen PowerShell."
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "Cargo was not found. Finish the Rust installation and reopen PowerShell."
}

Write-Host "Installing the bare-metal Rust target..." -ForegroundColor Cyan
& rustup target add x86_64-unknown-none
if ($LASTEXITCODE -ne 0) { throw "rustup target add failed with exit code $LASTEXITCODE" }

Write-Host "Rust toolchain ready." -ForegroundColor Green
Write-Host "Next: .\build.ps1, then .\run.ps1"

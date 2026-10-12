$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$iso = Join-Path $PSScriptRoot 'build\xavieros.iso'
$buildStamp = Join-Path $PSScriptRoot 'build\xavieros.build-ok'
if (-not (Test-Path $iso) -or -not (Test-Path $buildStamp)) {
    Write-Host 'A complete ISO was not found; rebuilding XavierOS...' -ForegroundColor Yellow
    & (Join-Path $PSScriptRoot 'build.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
    if (-not (Test-Path $iso) -or -not (Test-Path $buildStamp)) { throw 'Build did not produce a validated ISO.' }
}

$qemuCommand = Get-Command 'qemu-system-x86_64.exe' -ErrorAction SilentlyContinue
$qemu = if ($qemuCommand) { $qemuCommand.Source } else { $null }

if (-not $qemu) {
    $candidatePaths = @(
        (Join-Path $env:ProgramFiles 'qemu\qemu-system-x86_64.exe'),
        (Join-Path $env:ProgramFiles 'QEMU\qemu-system-x86_64.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'qemu\qemu-system-x86_64.exe')
    ) | Where-Object { $_ -and (Test-Path $_) }
    if ($candidatePaths.Count -gt 0) { $qemu = $candidatePaths[0] }
}

if (-not $qemu) {
    throw "QEMU was not found. Install QEMU for Windows from https://www.qemu.org/download/#windows, then run .\run.ps1 again."
}

Write-Host 'Starting XavierOS in QEMU. Close the QEMU window to stop it.' -ForegroundColor Cyan
# Keep the ISO argument ASCII/relative: some Windows QEMU builds mishandle Unicode argv paths.
# Set-Location above makes this path relative to the project root.
& $qemu -machine q35 -m 256M -cdrom 'build/xavieros.iso' -boot d -serial stdio -monitor none -no-shutdown

param(
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Assert-ExitCode([string]$CommandName) {
    if ($LASTEXITCODE -ne 0) {
        throw "$CommandName failed with exit code $LASTEXITCODE"
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "Cargo was not found. Install Rust with 'winget install --id Rustlang.Rustup -e' and reopen PowerShell."
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup was not found. Install Rust with 'winget install --id Rustlang.Rustup -e'."
}
if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
    throw "Windows tar.exe was not found. Use a current Windows 10/11 installation."
}

$buildDir = Join-Path $PSScriptRoot 'build'
$toolsDir = Join-Path $PSScriptRoot 'tools'
$limineDir = Join-Path $buildDir 'limine-binary'
$limineExtract = Join-Path $buildDir 'limine-extract'
$xorrisoExtract = Join-Path $toolsDir 'xorriso-win'
$buildStamp = Join-Path $buildDir 'xavieros.build-ok'

if ($Clean) {
    Remove-Item $buildDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $xorrisoExtract -Recurse -Force -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Path $buildDir -Force | Out-Null
New-Item -ItemType Directory -Path $toolsDir -Force | Out-Null
Remove-Item $buildStamp -Force -ErrorAction SilentlyContinue

Write-Host '[1/5] Checking Rust target...' -ForegroundColor Cyan
& rustup target add x86_64-unknown-none
Assert-ExitCode 'rustup target add'

# Fetch the official Limine binary release (includes a native Windows limine.exe).
$requiredLimineFiles = @('limine.exe', 'limine-bios.sys', 'limine-bios-cd.bin', 'limine-uefi-cd.bin', 'BOOTX64.EFI')
$limineReady = $true
foreach ($name in $requiredLimineFiles) {
    if (-not (Test-Path (Join-Path $limineDir $name))) { $limineReady = $false }
}
if (-not $limineReady) {
    Write-Host '[2/5] Downloading official Limine binaries...' -ForegroundColor Cyan
    $limineArchive = Join-Path $buildDir 'limine-binary.tar.gz'
    $limineUrl = 'https://github.com/Limine-Bootloader/Limine/releases/latest/download/limine-binary.tar.gz'
    Invoke-WebRequest -Uri $limineUrl -OutFile $limineArchive
    Remove-Item $limineExtract -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $limineExtract -Force | Out-Null
    & tar.exe -xzf $limineArchive -C $limineExtract
    Assert-ExitCode 'Extracting Limine'
    New-Item -ItemType Directory -Path $limineDir -Force | Out-Null

    foreach ($name in $requiredLimineFiles) {
        $found = Get-ChildItem -Path $limineExtract -Filter $name -File -Recurse | Select-Object -First 1
        if ($null -eq $found) { throw "The official Limine archive did not contain required file '$name'." }
        Copy-Item $found.FullName (Join-Path $limineDir $name) -Force
    }
}

# xorriso creates the bootable ISO. This Windows build includes its required DLLs.
$xorrisoExe = Get-ChildItem -Path $xorrisoExtract -Filter 'xorriso.exe' -File -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
if ($null -eq $xorrisoExe) {
    Write-Host '[3/5] Downloading the Windows xorriso bundle...' -ForegroundColor Cyan
    $xorrisoZip = Join-Path $toolsDir 'xorriso-win.zip'
    $xorrisoUrl = 'https://github.com/PeyTy/xorriso-exe-for-windows/archive/refs/heads/master.zip'
    Invoke-WebRequest -Uri $xorrisoUrl -OutFile $xorrisoZip
    Remove-Item $xorrisoExtract -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $xorrisoExtract -Force | Out-Null
    Expand-Archive -Path $xorrisoZip -DestinationPath $xorrisoExtract -Force
    $xorrisoExe = Get-ChildItem -Path $xorrisoExtract -Filter 'xorriso.exe' -File -Recurse | Select-Object -First 1
    if ($null -eq $xorrisoExe) { throw 'Could not find xorriso.exe in the downloaded bundle.' }
}

Write-Host '[4/5] Compiling XavierOS kernel...' -ForegroundColor Cyan
& cargo build --release --target x86_64-unknown-none
Assert-ExitCode 'cargo build'

$kernel = Join-Path $PSScriptRoot 'target\x86_64-unknown-none\release\xavieros'
if (-not (Test-Path $kernel)) { throw "Compiled kernel was not found at $kernel" }

$isoRoot = Join-Path $buildDir 'iso_root'
Remove-Item $isoRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $isoRoot 'boot\limine') -Force | Out-Null
Copy-Item $kernel (Join-Path $isoRoot 'boot\xavieros.elf') -Force
Copy-Item (Join-Path $PSScriptRoot 'limine.conf') (Join-Path $isoRoot 'boot\limine\limine.conf') -Force
Copy-Item (Join-Path $limineDir 'limine-bios.sys') (Join-Path $isoRoot 'boot\limine\limine-bios.sys') -Force
Copy-Item (Join-Path $limineDir 'limine-bios-cd.bin') (Join-Path $isoRoot 'boot\limine\limine-bios-cd.bin') -Force
Copy-Item (Join-Path $limineDir 'limine-uefi-cd.bin') (Join-Path $isoRoot 'boot\limine\limine-uefi-cd.bin') -Force
$efiBootDir = Join-Path $isoRoot 'EFI\BOOT'
New-Item -ItemType Directory -Path $efiBootDir -Force | Out-Null
Copy-Item (Join-Path $limineDir 'BOOTX64.EFI') (Join-Path $efiBootDir 'BOOTX64.EFI') -Force

Write-Host '[5/5] Building hybrid BIOS/UEFI ISO and installing the BIOS boot sector...' -ForegroundColor Cyan
$isoRelative = 'build/xavieros.iso'
Remove-Item $isoRelative -Force -ErrorAction SilentlyContinue
# xorriso 1.5.2 (the available Windows bundle) does not reliably resolve
# the --efi-boot-image placeholder as the source of an appended EFI partition.
# Pass the actual EFI image file to --efi-boot-part instead. It is the same file
# also referenced by --efi-boot for El Torito UEFI boot.
$efiPartitionSource = 'build/iso_root/boot/limine/limine-uefi-cd.bin'
if (-not (Test-Path $efiPartitionSource)) {
    throw "EFI boot image was not staged at '$efiPartitionSource'."
}

try {
    & $xorrisoExe.FullName -as mkisofs -R -r -J `
        -b 'boot/limine/limine-bios-cd.bin' -no-emul-boot -boot-load-size 4 -boot-info-table `
        -hfsplus -apm-block-size 2048 `
        --efi-boot 'boot/limine/limine-uefi-cd.bin' `
        --efi-boot-part $efiPartitionSource --protective-msdos-label `
        'build/iso_root' -o $isoRelative
    Assert-ExitCode 'xorriso'
} catch {
    Remove-Item (Join-Path $PSScriptRoot $isoRelative) -Force -ErrorAction SilentlyContinue
    throw
}

try {
    & (Join-Path $limineDir 'limine.exe') bios-install $isoRelative
    Assert-ExitCode 'limine bios-install'
} catch {
    # Never leave a partially-created ISO that run.ps1 might mistake for a valid build.
    Remove-Item (Join-Path $PSScriptRoot $isoRelative) -Force -ErrorAction SilentlyContinue
    throw
}

Set-Content -Path $buildStamp -Value 'Build completed successfully.' -Encoding ASCII
Write-Host "`nBuild complete: $isoRelative" -ForegroundColor Green
Write-Host 'Run it with: .\run.ps1'

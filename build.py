#!/usr/bin/env python3
"""Build a BIOS/UEFI bootable XavierOS ISO on Windows, macOS, or Linux.

Requirements: Python 3.10+, Rust/rustup, xorriso, and an executable Limine
utility contained in the official Limine binary release archive. The script
bootstraps Limine and (on Windows) the existing xorriso bundle if needed.
"""
from __future__ import annotations

import argparse
import os
import shutil
import stat
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BUILD = ROOT / "build"
TOOLS = ROOT / "tools"
LIMINE_DIR = BUILD / "limine-binary"
LIMINE_EXTRACT = BUILD / "limine-extract"
XORRISO_EXTRACT = TOOLS / "xorriso-win"
LIMINE_URL = "https://github.com/Limine-Bootloader/Limine/releases/latest/download/limine-binary.tar.gz"
XORRISO_WINDOWS_URL = "https://github.com/PeyTy/xorriso-exe-for-windows/archive/refs/heads/master.zip"
REQUIRED_BOOT_FILES = (
    "limine-bios.sys",
    "limine-bios-cd.bin",
    "limine-uefi-cd.bin",
    "BOOTX64.EFI",
)


def run(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess:
    print("+", subprocess.list2cmdline(command) if os.name == "nt" else " ".join(command), flush=True)
    return subprocess.run(command, cwd=ROOT, check=check)


def download(url: str, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    print(f"Downloading {url}", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "XavierOS-build/0.2.1"})
    with urllib.request.urlopen(request, timeout=60) as response, destination.open("wb") as out:
        shutil.copyfileobj(response, out)


def find_named(directory: Path, name: str) -> Path | None:
    if not directory.exists():
        return None
    return next((p for p in directory.rglob("*") if p.is_file() and p.name.lower() == name.lower()), None)


def _safe_archive_parts(name: str) -> tuple[str, ...]:
    """Return a portable relative path, rejecting traversal and platform-specific escapes."""
    if not name or "\x00" in name or "\\" in name or name.startswith("/"):
        raise RuntimeError(f"Unsafe path in archive: {name!r}")
    raw_parts = name.split("/")
    if any(part == ".." for part in raw_parts) or any(":" in part for part in raw_parts):
        raise RuntimeError(f"Unsafe path in archive: {name!r}")
    return tuple(part for part in raw_parts if part not in ("", "."))


def _safe_target(destination: Path, parts: tuple[str, ...], original_name: str) -> Path:
    root_resolved = destination.resolve()
    target = destination.joinpath(*parts)
    resolved = target.resolve()
    if resolved != root_resolved and root_resolved not in resolved.parents:
        raise RuntimeError(f"Unsafe path in archive: {original_name}")
    return target


def safe_extract_tar(archive: Path, destination: Path) -> None:
    """Extract regular files/directories only; never follow archive-provided links."""
    destination.mkdir(parents=True, exist_ok=True)
    seen: set[tuple[str, ...]] = set()
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        prepared: list[tuple[tarfile.TarInfo, tuple[str, ...]]] = []
        for member in members:
            parts = _safe_archive_parts(member.name)
            if not parts:
                if member.isdir():
                    continue
                raise RuntimeError(f"Archive member has no safe destination: {member.name!r}")
            if not (member.isdir() or member.isfile()):
                raise RuntimeError(f"Links and special files are not accepted in archives: {member.name}")
            key = tuple(part.casefold() for part in parts)
            if key in seen:
                raise RuntimeError(f"Duplicate archive path: {member.name}")
            seen.add(key)
            _safe_target(destination, parts, member.name)
            prepared.append((member, parts))

        # Create directories first, then stream regular files manually. No archive
        # member is allowed to create a symlink/hardlink that later members follow.
        for member, parts in prepared:
            if member.isdir():
                _safe_target(destination, parts, member.name).mkdir(parents=True, exist_ok=True)
        for member, parts in prepared:
            if not member.isfile():
                continue
            target = _safe_target(destination, parts, member.name)
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_symlink():
                raise RuntimeError(f"Refusing to overwrite a symbolic link: {member.name}")
            source = tar.extractfile(member)
            if source is None:
                raise RuntimeError(f"Could not read archive member: {member.name}")
            with source, target.open("wb") as output:
                shutil.copyfileobj(source, output)


def safe_extract_zip(archive: Path, destination: Path) -> None:
    """Extract ZIP files without traversal, link, or case-insensitive path tricks."""
    destination.mkdir(parents=True, exist_ok=True)
    seen: set[tuple[str, ...]] = set()
    with zipfile.ZipFile(archive) as source:
        prepared: list[tuple[zipfile.ZipInfo, tuple[str, ...], bool]] = []
        for member in source.infolist():
            parts = _safe_archive_parts(member.filename)
            is_dir = member.is_dir()
            mode = (member.external_attr >> 16) & 0xFFFF
            file_type = stat.S_IFMT(mode)
            if file_type == stat.S_IFLNK or file_type not in (0, stat.S_IFREG, stat.S_IFDIR):
                raise RuntimeError(f"Links and special files are not accepted in archives: {member.filename}")
            if not parts:
                if is_dir:
                    continue
                raise RuntimeError(f"Archive member has no safe destination: {member.filename!r}")
            key = tuple(part.casefold() for part in parts)
            if key in seen:
                raise RuntimeError(f"Duplicate archive path: {member.filename}")
            seen.add(key)
            _safe_target(destination, parts, member.filename)
            prepared.append((member, parts, is_dir))

        for member, parts, is_dir in prepared:
            if is_dir:
                _safe_target(destination, parts, member.filename).mkdir(parents=True, exist_ok=True)
        for member, parts, is_dir in prepared:
            if is_dir:
                continue
            target = _safe_target(destination, parts, member.filename)
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_symlink():
                raise RuntimeError(f"Refusing to overwrite a symbolic link: {member.filename}")
            with source.open(member) as input_stream, target.open("wb") as output:
                shutil.copyfileobj(input_stream, output)


def ensure_limine() -> tuple[Path, Path]:
    limine_program_names = ("limine.exe", "limine") if os.name == "nt" else ("limine", "limine.exe")
    program = None
    for name in limine_program_names:
        program = find_named(LIMINE_DIR, name)
        if program:
            break
        installed = shutil.which(name)
        if installed:
            program = Path(installed)
            break
    files_missing = [name for name in REQUIRED_BOOT_FILES if not find_named(LIMINE_DIR, name)]
    if program and not files_missing:
        if os.name != "nt":
            program.chmod(program.stat().st_mode | 0o111)
        return program, LIMINE_DIR

    archive = BUILD / "limine-binary.tar.gz"
    extract_dir = LIMINE_EXTRACT
    BUILD.mkdir(parents=True, exist_ok=True)
    download(LIMINE_URL, archive)
    shutil.rmtree(extract_dir, ignore_errors=True)
    safe_extract_tar(archive, extract_dir)
    LIMINE_DIR.mkdir(parents=True, exist_ok=True)

    found_program = None
    for name in limine_program_names:
        found_program = find_named(extract_dir, name)
        if found_program:
            break

    if found_program:
        if os.name != "nt":
            found_program.chmod(found_program.stat().st_mode | 0o111)
        program_destination = LIMINE_DIR / found_program.name
        shutil.copy2(found_program, program_destination)
    elif os.name != "nt":
        # The upstream binary archive contains the official limine.c utility
        # source on Unix-like hosts (and a prebuilt .exe for Windows). Compile
        # that host tool with the system C compiler rather than depending on a
        # Linux-only or PowerShell-based build path.
        source = find_named(extract_dir, "limine.c")
        compiler = shutil.which("cc") or shutil.which("clang") or shutil.which("gcc")
        if source is None or compiler is None:
            raise RuntimeError(
                "No host-native Limine executable was found. Install a C compiler (cc/clang/gcc), "
                "or install the Limine utility and put it on PATH."
            )
        program_destination = LIMINE_DIR / "limine"
        run([compiler, "-O2", "-std=c99", "-D_FILE_OFFSET_BITS=64", str(source), "-o", str(program_destination)])
        program_destination.chmod(program_destination.stat().st_mode | 0o111)
    else:
        raise RuntimeError(
            "The official Limine archive did not contain limine.exe. "
            "Place a Windows Limine utility in build/limine-binary/ and retry."
        )

    for name in REQUIRED_BOOT_FILES:
        found = find_named(extract_dir, name)
        if not found:
            raise RuntimeError(f"The official Limine archive is missing required file: {name}")
        shutil.copy2(found, LIMINE_DIR / name)
    return program_destination, LIMINE_DIR


def ensure_xorriso() -> Path:
    installed = shutil.which("xorriso")
    if installed:
        return Path(installed)
    # Keep support for the already-used Windows bundle while making the normal
    # path rely on each platform's package manager rather than PowerShell.
    bundled = find_named(XORRISO_EXTRACT, "xorriso.exe") if os.name == "nt" else None
    if bundled:
        return bundled
    if os.name == "nt":
        archive = TOOLS / "xorriso-win.zip"
        download(XORRISO_WINDOWS_URL, archive)
        shutil.rmtree(XORRISO_EXTRACT, ignore_errors=True)
        XORRISO_EXTRACT.mkdir(parents=True, exist_ok=True)
        safe_extract_zip(archive, XORRISO_EXTRACT)
        bundled = find_named(XORRISO_EXTRACT, "xorriso.exe")
        if bundled:
            return bundled
    if sys.platform == "darwin":
        hint = "Install it with Homebrew: brew install xorriso"
    elif os.name == "nt":
        hint = "Install xorriso and add it to PATH, or retry so the Windows bundle can be downloaded."
    else:
        hint = "Install it with your package manager (for example: sudo apt install xorriso)."
    raise RuntimeError(f"xorriso was not found. {hint}")


def clean_outputs() -> None:
    shutil.rmtree(BUILD, ignore_errors=True)
    # Downloaded Windows xorriso fallback can be regenerated; do not delete all tools.
    shutil.rmtree(XORRISO_EXTRACT, ignore_errors=True)
    (TOOLS / "xorriso-win.zip").unlink(missing_ok=True)


def build_iso() -> Path:
    cargo = shutil.which("cargo")
    rustup = shutil.which("rustup")
    if not cargo or not rustup:
        raise RuntimeError("Rust/cargo/rustup is required. Install Rust from https://rustup.rs/ and reopen the terminal.")
    run([rustup, "target", "add", "x86_64-unknown-none"])
    limine_program, limine_dir = ensure_limine()
    xorriso = ensure_xorriso()

    print("[1/4] Building the read-only initramfs from rootfs/", flush=True)
    run([sys.executable, str(ROOT / "mkinitramfs.py")])
    print("[2/4] Compiling the freestanding Rust kernel", flush=True)
    run([cargo, "build", "--release", "--target", "x86_64-unknown-none"])
    kernel = ROOT / "target" / "x86_64-unknown-none" / "release" / ("xavieros.exe" if os.name == "nt" else "xavieros")
    # Cargo's bare-metal output normally has no .exe suffix even when built from Windows.
    if not kernel.exists():
        kernel = ROOT / "target" / "x86_64-unknown-none" / "release" / "xavieros"
    if not kernel.is_file():
        raise RuntimeError(f"Kernel ELF not found after cargo build: {kernel}")

    iso_root = BUILD / "iso_root"
    shutil.rmtree(iso_root, ignore_errors=True)
    boot_limine = iso_root / "boot" / "limine"
    efi_boot = iso_root / "EFI" / "BOOT"
    boot_limine.mkdir(parents=True, exist_ok=True)
    efi_boot.mkdir(parents=True, exist_ok=True)
    shutil.copy2(kernel, iso_root / "boot" / "xavieros.elf")
    shutil.copy2(ROOT / "limine.conf", boot_limine / "limine.conf")
    for name in REQUIRED_BOOT_FILES:
        found = find_named(limine_dir, name)
        if not found:
            raise RuntimeError(f"Missing Limine boot file: {name}")
        if name == "BOOTX64.EFI":
            shutil.copy2(found, efi_boot / name)
        else:
            shutil.copy2(found, boot_limine / name)

    print("[3/4] Creating the hybrid BIOS/UEFI ISO", flush=True)
    iso = BUILD / "xavieros.iso"
    iso.unlink(missing_ok=True)
    iso_relative = "build/xavieros.iso"
    efi_source = "build/iso_root/boot/limine/limine-uefi-cd.bin"
    mkisofs_args = [
        str(xorriso), "-as", "mkisofs", "-R", "-r", "-J",
        "-b", "boot/limine/limine-bios-cd.bin", "-no-emul-boot",
        "-boot-load-size", "4", "-boot-info-table", "-hfsplus",
        "-apm-block-size", "2048", "--efi-boot", "boot/limine/limine-uefi-cd.bin",
        "--efi-boot-part", efi_source, "--protective-msdos-label",
        "build/iso_root", "-o", iso_relative,
    ]
    run(mkisofs_args)
    print("[4/4] Installing the Limine BIOS boot code", flush=True)
    run([str(limine_program), "bios-install", iso_relative])
    (BUILD / "xavieros.build-ok").write_text("Build completed successfully.\n", encoding="ascii")
    return iso


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clean", action="store_true", help="remove build output before building")
    args = parser.parse_args()
    if args.clean:
        clean_outputs()
    (BUILD / "xavieros.build-ok").unlink(missing_ok=True)
    try:
        iso = build_iso()
    except (OSError, RuntimeError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as exc:
        (BUILD / "xavieros.iso").unlink(missing_ok=True)
        print(f"build failed: {exc}", file=sys.stderr)
        return getattr(exc, "returncode", 1) or 1
    print(f"\nBuild successful: {iso.relative_to(ROOT)}")
    print("Run with: python run.py")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

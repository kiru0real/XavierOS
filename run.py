#!/usr/bin/env python3
"""Build if needed and launch XavierOS in QEMU, on Windows/macOS/Linux."""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ISO = ROOT / "build" / "xavieros.iso"
STAMP = ROOT / "build" / "xavieros.build-ok"
DISK = ROOT / "data" / "xavieros-disk.img"
DISK_SIZE_MIB = 16


def find_qemu() -> str | None:
    names = ("qemu-system-x86_64.exe", "qemu-system-x86_64") if os.name == "nt" else ("qemu-system-x86_64", "qemu-system-x86_64.exe")
    for name in names:
        found = shutil.which(name)
        if found:
            return found
    if os.name == "nt":
        candidates = []
        for variable in ("ProgramFiles", "ProgramFiles(x86)"):
            base = os.environ.get(variable)
            if base:
                candidates.extend((Path(base) / "qemu" / "qemu-system-x86_64.exe", Path(base) / "QEMU" / "qemu-system-x86_64.exe"))
        for candidate in candidates:
            if candidate.is_file():
                return str(candidate)
    return None


def needs_rebuild() -> bool:
    if not ISO.is_file() or not STAMP.is_file():
        return True
    inputs = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "limine.conf", ROOT / "linker.ld", ROOT / "build.py", ROOT / "setup.py", ROOT / "mkinitramfs.py"]
    cargo_config = ROOT / ".cargo"
    if cargo_config.exists():
        inputs.extend(path for path in cargo_config.rglob("*") if path.is_file())
    src_dir = ROOT / "src"
    if src_dir.exists():
        inputs.extend(path for path in src_dir.rglob("*") if path.is_file())
    rootfs_dir = ROOT / "rootfs"
    if rootfs_dir.exists():
        inputs.extend(path for path in rootfs_dir.rglob("*") if path.is_file())
    generated_fs = ROOT / "initramfs.xfs"
    if generated_fs.exists():
        inputs.append(generated_fs)
    newest_input = max((path.stat().st_mtime for path in inputs if path.is_file()), default=0)
    return newest_input > STAMP.stat().st_mtime


def ensure_disk_image(reset: bool = False) -> Path:
    DISK.parent.mkdir(parents=True, exist_ok=True)
    if reset and DISK.exists():
        DISK.unlink()
    if not DISK.exists():
        with DISK.open("wb") as stream:
            stream.truncate(DISK_SIZE_MIB * 1024 * 1024)
        print(f"Created a blank {DISK_SIZE_MIB} MiB persistent disk: {DISK.relative_to(ROOT)}", flush=True)
    elif DISK.stat().st_size < 1024 * 1024:
        raise RuntimeError("The existing disk image is unexpectedly small. Use --reset-disk to replace it (this erases its contents).")
    return DISK


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reset-disk", action="store_true", help="erase and recreate the persistent 16 MiB virtual disk")
    parser.add_argument("--no-disk", action="store_true", help="run without attaching the persistent writable disk")
    args = parser.parse_args()
    if needs_rebuild():
        print("A missing or out-of-date ISO was detected; building XavierOS first.", flush=True)
        result = subprocess.run([sys.executable, str(ROOT / "build.py")], cwd=ROOT, check=False)
        if result.returncode:
            return result.returncode
    qemu = find_qemu()
    if not qemu:
        if sys.platform == "darwin":
            hint = "Install QEMU with: brew install qemu"
        elif os.name == "nt":
            hint = "Install QEMU for Windows and add qemu-system-x86_64.exe to PATH."
        else:
            hint = "Install QEMU with your package manager (for example: sudo apt install qemu-system-x86)."
        print(f"error: QEMU was not found. {hint}", file=sys.stderr)
        return 1
    print("Starting XavierOS in QEMU; close the emulator window to stop it.", flush=True)
    # Legacy PC/PIIX exposes ATA PIO; QEMU user-mode NAT adds an E1000 NIC, and -cpu max exposes RDRAND when TCG supports it.
    command = [qemu, "-machine", "pc", "-cpu", "max", "-rtc", "base=utc", "-m", "256M", "-cdrom", "build/xavieros.iso", "-boot", "d",
               "-netdev", "user,id=xvnet0", "-device", "e1000,netdev=xvnet0"]
    if not args.no_disk:
        try:
            disk = ensure_disk_image(args.reset_disk)
        except (OSError, RuntimeError) as exc:
            print(f"error: could not prepare disk image: {exc}", file=sys.stderr)
            return 1
        command.extend(["-drive", f"file={disk.relative_to(ROOT).as_posix()},format=raw,if=ide,index=0,media=disk"])
        print(f"Persistent disk attached: {disk.relative_to(ROOT)} ({disk.stat().st_size // (1024 * 1024)} MiB)", flush=True)
    command.extend(["-serial", "stdio", "-monitor", "none", "-no-shutdown"])
    return subprocess.run(command, cwd=ROOT, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())

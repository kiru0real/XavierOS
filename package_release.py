#!/usr/bin/env python3
"""Prepare versioned ISO and SHA-256 sums for a manual GitHub release upload."""
from __future__ import annotations

import argparse
import hashlib
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent
VERSION = "0.1.2"
DEFAULT_ISO = ROOT / "build" / "xavieros.iso"
DEFAULT_OUTPUT = ROOT / "dist"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package_release(iso_path: Path, output_dir: Path) -> tuple[Path, Path]:
    if not iso_path.is_file():
        raise FileNotFoundError(f"ISO not found: {iso_path}")
    if iso_path.stat().st_size == 0:
        raise ValueError("ISO is empty")

    output_dir.mkdir(parents=True, exist_ok=True)
    iso_name = f"xavieros-{VERSION}.iso"
    output_iso = output_dir / iso_name
    if iso_path.resolve() != output_iso.resolve():
        shutil.copy2(iso_path, output_iso)

    checksum_file = output_dir / "SHA256SUMS.txt"
    checksum_file.write_text(f"{sha256_file(output_iso)}  {iso_name}\n", encoding="ascii", newline="\n")
    return output_iso, checksum_file


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iso", type=Path, default=DEFAULT_ISO, help="source ISO (default: build/xavieros.iso)")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT, help="output directory (default: dist/)")
    args = parser.parse_args()
    iso_path = args.iso if args.iso.is_absolute() else ROOT / args.iso
    output_dir = args.output if args.output.is_absolute() else ROOT / args.output
    try:
        output_iso, checksum_file = package_release(iso_path, output_dir)
    except (OSError, ValueError) as exc:
        parser.exit(1, f"release packaging failed: {exc}\n")
    print(f"Prepared ISO: {output_iso.relative_to(ROOT) if output_iso.is_relative_to(ROOT) else output_iso}")
    print(f"Prepared checksum: {checksum_file.relative_to(ROOT) if checksum_file.is_relative_to(ROOT) else checksum_file}")
    print("Upload both files to the GitHub Release Assets section.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

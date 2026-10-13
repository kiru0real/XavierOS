#!/usr/bin/env python3
"""Build XavierOS's read-only XVRFS v1 initramfs from rootfs/ assets."""
from __future__ import annotations
import argparse
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ROOTFS = ROOT / "rootfs"
OUTPUT = ROOT / "initramfs.xfs"
MAGIC = b"XVRFS001"
HEADER = struct.Struct("<8sII")
RECORD = struct.Struct("<B3xIHHII")
KIND_DIRECTORY = 1
KIND_FILE = 2


def build(rootfs: Path, output: Path) -> None:
    if not rootfs.is_dir():
        raise FileNotFoundError(f"root filesystem directory not found: {rootfs}")
    items: list[tuple[int, str, bytes]] = []
    for path in rootfs.rglob("*"):
        rel = path.relative_to(rootfs).as_posix()
        canonical = rel.upper()
        if not canonical or canonical.startswith("/") or "\\" in canonical or any(p in ("", ".", "..") for p in canonical.split("/")):
            raise ValueError(f"invalid rootfs path: {rel}")
        if len(canonical.encode("utf-8")) > 1024:
            raise ValueError(f"path too long: {rel}")
        if path.is_symlink():
            raise ValueError(f"symlinks are not supported: {path}")
        if path.is_dir():
            items.append((KIND_DIRECTORY, canonical, b""))
        elif path.is_file():
            items.append((KIND_FILE, canonical, path.read_bytes()))
        else:
            raise ValueError(f"unsupported filesystem entry: {path}")
    items.sort(key=lambda item: (item[1].casefold(), item[0], item[1]))
    if not items:
        raise ValueError("rootfs contains no entries")
    seen: set[str] = set()
    for _, path, _ in items:
        key = path.casefold()
        if key in seen:
            raise ValueError(f"case-insensitive path collision: {path}")
        seen.add(key)

    table_offset = HEADER.size
    data_cursor = table_offset + len(items) * RECORD.size
    path_blobs: list[bytes] = []
    path_offsets: list[int] = []
    for _, path, _ in items:
        blob = path.encode("utf-8")
        path_offsets.append(data_cursor)
        path_blobs.append(blob)
        data_cursor += len(blob)

    file_offsets: list[int] = []
    for kind, _, content in items:
        if kind == KIND_FILE:
            file_offsets.append(data_cursor)
            data_cursor += len(content)
        else:
            file_offsets.append(0)

    if data_cursor > 0xFFFFFFFF:
        raise ValueError("initramfs image exceeds 4 GiB format limit")
    image = bytearray(HEADER.pack(MAGIC, len(items), table_offset))
    for (kind, _path, content), path_offset, path_blob, data_offset in zip(items, path_offsets, path_blobs, file_offsets):
        image.extend(RECORD.pack(kind, path_offset, len(path_blob), 0, data_offset, len(content)))
    for path_blob in path_blobs:
        image.extend(path_blob)
    for (kind, _path, content), data_offset in zip(items, file_offsets):
        if kind == KIND_FILE:
            if len(image) != data_offset:
                raise RuntimeError("internal archive offset mismatch")
            image.extend(content)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(image)
    print(f"Built {output.name}: {len(items)} entries, {len(image)} bytes")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOTFS, help="source directory (default: rootfs/)")
    parser.add_argument("--output", type=Path, default=OUTPUT, help="output image (default: initramfs.xfs)")
    args = parser.parse_args()
    root = args.root if args.root.is_absolute() else ROOT / args.root
    output = args.output if args.output.is_absolute() else ROOT / args.output
    try:
        build(root, output)
    except (OSError, ValueError, RuntimeError) as exc:
        parser.exit(1, f"initramfs build failed: {exc}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

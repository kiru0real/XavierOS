# XVRFS v1 Image Format

XVRFS v1 is XavierOS's read-only initramfs format. `mkinitramfs.py` builds `initramfs.xfs` from `rootfs/`; the kernel validates and reads the image without heap allocation.

All integers are little-endian. Offsets are byte offsets from the beginning of the image.

## Header — 16 bytes

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | Magic: ASCII `XVRFS001` |
| 8 | 4 | Entry count (`u32`) |
| 12 | 4 | Table offset (`u32`, currently 16) |

## Entry record — 20 bytes

The table contains `entry_count` consecutive records starting at `table_offset`.

| Record offset | Size | Field |
|---:|---:|---|
| 0 | 1 | Type: `1` directory, `2` file |
| 1 | 3 | Reserved, zero |
| 4 | 4 | Path offset (`u32`) |
| 8 | 2 | Path length (`u16`) |
| 10 | 2 | Reserved, zero |
| 12 | 4 | Data offset (`u32`) |
| 16 | 4 | Data length (`u32`) |

Paths are UTF-8, relative to root, slash-separated, and stored without a leading slash. The current generator canonicalizes paths to uppercase to match shell input. Directory entries have zero data length; file payloads follow the path strings.

## Limitations

The image is embedded into the kernel ELF at build time and is read-only. There is no write support, permissions, timestamps, hard links, symlinks or persistent storage in XVRFS itself. The format includes bounds validation but is intended for trusted build input, not hostile media.

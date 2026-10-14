# XVDISKFS v1 — Writable Disk Filesystem

XVDISKFS v1 is a small experimental writable filesystem for the primary ATA PIO disk used by the XavierOS QEMU runner. The disk image is `data/xavieros-disk.img` and is excluded from Git. The filesystem is not crash-safe; use only disposable virtual disks.

All integers are little-endian. Disk addresses are 512-byte LBA sectors.

## Disk layout

| LBA range | Purpose |
|---:|---|
| `0` | Superblock |
| `1..16` | Metadata table: 16 sectors, 128 records × 64 bytes |
| `17..31` | Reserved |
| `32..end` | File payloads |

The superblock fields are:

| Byte offset | Size | Field |
|---:|---:|---|
| `0` | 8 | Magic bytes `XVDISK01` |
| `8` | 4 | Version (`1`) |
| `12` | 4 | Declared disk size in sectors |
| `16` | 4 | Metadata start LBA (`1`) |
| `20` | 4 | Metadata size in sectors (`16`) |
| `24` | 4 | Data-region start LBA (`32`) |
| `28` | 4 | Maximum record count (`128`) |
| `32` | 4 | Checksum over bytes `0..32` (rotate-left-by-3 XOR per byte, seeded with `0xA5A55A5A`) |

Each 64-byte record contains:

| Record offset | Size | Field |
|---:|---:|---|
| `0` | 1 | Kind (`0` free, `1` directory, `2` file) |
| `1` | 1 | Name length (`0..48`) |
| `2` | 2 | Parent record index (`u16`) |
| `4` | 4 | File size in bytes (`u32`) |
| `8` | 4 | First data LBA (`u32`, files only) |
| `12` | 4 | Allocated sector count (`u32`, files only) |
| `16` | 48 | Printable ASCII name; unused bytes are zero |

Record 0 is the root directory. Directories refer to a parent record. Each file uses one contiguous extent; allocation scans extents for a sufficiently large gap. A replacement temporarily keeps the old extent reserved while writing new data, then updates metadata.

## Limitations and cautions

- Fixed table of 128 records including root.
- One contiguous extent per file; fragmentation can prevent an allocation even if free sectors remain.
- Names are 1–48 printable ASCII bytes; path depth is limited to 16.
- No permissions in the on-disk format, timestamps, links, journal, per-record/payload checksums, or power-loss recovery.
- Synchronous polling I/O on legacy primary-master ATA LBA28.
- Auto-initialization of a blank all-zero superblock is destructive to any existing contents on that target device. Never attach a disk with valuable data.
- `FORMAT DISK` and `python run.py --reset-disk` are destructive.

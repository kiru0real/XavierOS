# XVDISKFS v1 — writable disk filesystem

XVDISKFS v1 is a small, experimental, writable filesystem for the primary ATA PIO disk used by the XavierOS QEMU runner. The disk image is created locally at `data/xavieros-disk.img` and is excluded from Git. It is not crash-safe and must only be used with a disposable virtual disk. On boot, the driver auto-initializes metadata if LBA0 is all zero; this checks only the superblock sector, not every sector on the device, so never attach a disk containing valuable data.

All integers are little-endian. Disk addresses are 512-byte LBA sectors.

## Disk layout

| LBA range | Purpose |
|---:|---|
| `0` | Superblock |
| `1..16` | 16-sector metadata table (128 records × 64 bytes) |
| `17..31` | Reserved |
| `32..end` | File payloads |

The superblock sector fields are:

| Byte offset | Size | Field |
|---:|---:|---|
| `0` | 8 | Magic bytes `XVDISK01` |
| `8` | 4 | Version (`1`) |
| `12` | 4 | Declared disk size in sectors |
| `16` | 4 | Metadata start LBA (`1`) |
| `20` | 4 | Metadata size in sectors (`16`) |
| `24` | 4 | Data region start LBA (`32`) |
| `28` | 4 | Maximum record count (`128`) |
| `32` | 4 | Checksum over bytes `0..32` (rotate-left-by-3 XOR each byte, seeded with `0xA5A55A5A`) |

Each 64-byte entry contains:

| Record offset | Size | Field |
|---:|---:|---|
| `0` | 1 | Kind (`0` free, `1` directory, `2` file) |
| `1` | 1 | Name length (`0..48`) |
| `2` | 2 | Parent record index (`u16`) |
| `4` | 4 | File size in bytes (`u32`) |
| `8` | 4 | First data LBA (`u32`, files only) |
| `12` | 4 | Allocated sector count (`u32`, files only) |
| `16` | 48 | Name bytes, printable ASCII, unused bytes zero |

Record `0` is the root directory. Directory entries refer to their parent record. Files use one contiguous extent; allocation scans existing extents for the first gap large enough. A replacement keeps the old extent reserved while writing new data, then updates the metadata entry. This requires enough spare contiguous space for the replacement and does not provide transactional recovery or a write-ahead log.

## Current limitations

- Fixed table with 128 total records, including the root directory.
- One contiguous extent per file; a fragmented disk can report “disk full or too fragmented” even when some space remains.
- Names are 1–48 printable ASCII bytes; path depth is limited to 16.
- No permissions, timestamps, links, journal, checksums for metadata records/payload sectors, or recovery from power loss.
- All I/O is synchronous and uses polling on legacy primary-master ATA LBA28.
- `CAT` uses a 4 KiB console buffer. The current command shell can write only short text payloads in a single command.
- `FORMAT DISK` is destructive. `python run.py --reset-disk` also erases and recreates the entire disk image.

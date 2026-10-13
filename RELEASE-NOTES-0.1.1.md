# XavierOS 0.1.1

XavierOS 0.1.1 is a maintenance update focused on filesystem integrity checks, kernel heap validation, ATA I/O completion handling, build-tool safety, and repository testing infrastructure.

## Fixes and hardening

- Prevent file replacement from allocating on top of the old file's storage before replacement metadata is written.
- Validate XVDISKFS root metadata, storage fields, parent chains, directory depth, and LBA extents when mounting.
- Fix an extent allocator scan edge case that could use stale scan results when metadata is not ordered by disk position.
- Validate heap pointer bounds and allocation metadata during deallocation; reject free-range overlaps and common double-free corruption cases.
- Confirm ATA read/write command completion after transferring sector data.
- Reject traversal paths, links, special files, and duplicate case-insensitive paths when extracting downloaded build-tool archives.

## Developer experience

- Add host-side unit tests for archive extraction, initramfs generation, and release packaging.
- Add GitHub Actions CI for Python checks, initramfs reproducibility, and freestanding Rust compilation.
- Add a script to prepare a versioned ISO and SHA-256 checksum file.
- Expand architecture, contribution, security, test, and roadmap documentation.

## Known limitations

- XVDISKFS is not journaled and is not crash-safe. Use disposable QEMU disk images only.
- Storage targets the QEMU legacy PC/PIIX IDE interface using synchronous ATA PIO.
- Task switching is cooperative; timer preemption, user mode, address-space isolation, networking, and a desktop are not implemented.
- Physical hardware is not validated by the CI workflow.

## Before publication

The release maintainer should confirm that CI is green, `python build.py --clean` succeeds, `SELFTEST` passes in QEMU, and a file survives a clean QEMU shutdown/restart on the disposable virtual disk.

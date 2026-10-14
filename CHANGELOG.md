# Changelog

Notable changes to XavierOS are documented here. The GitHub release `v0.1.0` remains immutable; `0.1.1` is the maintenance source snapshot being prepared for validation.

## 0.1.1 — Maintenance update (pending local validation)

### Fixed

- File replacement no longer lets the extent allocator reuse a file's currently referenced sectors before new data and metadata are committed. Replacement needs spare contiguous space; if there is not enough room, it fails rather than risking the old file's contents.
- The writable filesystem is unmounted before a new disk initialization/mount attempt, preventing stale metadata from being reported as available after a failed retry.
- XVDISKFS mount validation now checks root metadata, empty-file and directory storage fields, parent chains, directory depth/cycles, and existing extent bounds/overlap checks.
- The disk extent allocator restarts its search after moving past an occupied extent, preventing a stale scan result from moving the candidate backwards when metadata entries are not ordered by LBA.
- ATA reads/writes check command status after the data phase before reporting success.
- Heap deallocation validates pointer bounds and allocation-header fields, and refuses to reinsert ranges that overlap blocks already present in the free list.
- Build-tool extraction refuses traversal paths, symbolic/hard links, special files, and duplicate case-insensitive paths in downloaded Limine/xorriso archives.

### Added

- Host-side unit tests for safe archive extraction, initramfs format generation, path-collision rejection, and release packaging checksums.
- `package_release.py`, which copies `build/xavieros.iso` to `dist/xavieros-0.1.1.iso` and generates `dist/SHA256SUMS.txt` for manual upload to GitHub Releases.
- Architecture, testing, roadmap, contribution, security, and release-update documentation plus issue/PR templates.

### Notes

- This source snapshot still requires a clean local Rust build and QEMU regression test before publication.
- The filesystem is not crash-safe; the new guards reduce specific failure modes but do not provide journaling or recovery.
- The scheduler remains cooperative. Preemption, user mode, process isolation, networking, and a desktop are not implemented.

## 0.1.0 — Initial public release

- Freestanding `no_std` Rust kernel targeting `x86_64-unknown-none`, booted through Limine.
- Framebuffer console and interactive shell.
- PMM, HHDM, 1 MiB kernel heap, IDT, CPU exception diagnostics, PIC/PIT, and PS/2 keyboard IRQ handling.
- Read-only XVRFS v1 initramfs embedded into the kernel.
- ATA PIO driver for the QEMU primary IDE master and writable XVDISKFS v1 mounted under `/DISK`.
- VFS routing and a cooperative task-context switching prototype.
- Cross-platform Python tooling for preparing, building, and running XavierOS under QEMU.

## Earlier development foundations

- Limine memory map support and bitmap PMM for usable pages below 4 GiB.
- HHDM translation and memory allocator diagnostics.
- Shell commands for system information, disk/filesystem access, task checks, and self-tests.
- Portable build/run scripts to replace Windows-only PowerShell as the primary documented workflow.

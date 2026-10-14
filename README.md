<div align="center">

# XavierOS

**A small experimental x86_64 operating system written in Rust.**

A `no_std` kernel booted by Limine, with a framebuffer console, memory management, CPU exception diagnostics, interrupt-driven keyboard input, an embedded initramfs, and a small writable filesystem for a QEMU virtual disk.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Target-x86__64-informational)](#current-features)
[![Status](https://img.shields.io/badge/Status-Experimental-orange)](#status-and-limitations)

**Source version: `0.1.1` · Latest published release: [`0.1.1`](https://github.com/kiru0real/XavierOS/releases/tag/v0.1.1) **

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a learning-focused hobby operating system. It is written in Rust without the standard library (`#![no_std]`) and currently targets `x86_64-unknown-none`. It boots to its own text console; there is no desktop environment yet.

The `main` branch contains the source snapshot for the planned `0.1.1` maintenance update. That update is **not considered release-ready until the kernel build and QEMU tests pass**. The existing `v0.1.0` GitHub Release remains unchanged.

## Current features

- Limine boot protocol and a hybrid BIOS/UEFI ISO build path.
- Framebuffer console and custom command shell.
- Limine memory-map parsing, higher-half direct map (HHDM), bitmap physical memory manager (PMM), and a 1 MiB first-fit kernel heap.
- x86_64 IDT and CPU exception diagnostics, including general-purpose registers and CR2 for page faults.
- Legacy PIC/PIT setup, timer tick counter, and PS/2 keyboard input via IRQ1 and a bounded ring buffer.
- Read-only XVRFS v1 initramfs embedded in the kernel image.
- ATA PIO driver for the QEMU primary IDE master and writable XVDISKFS v1 at `/DISK`.
- VFS routing between embedded files and the writable disk filesystem.
- Cooperative task-context switching prototype with independent task stacks. This is **not** timer-preemptive multitasking.
- Self-tests and diagnostics for memory, heap, interrupts, filesystem mounts, disk metadata, and cooperative task switching.
- Cross-platform Python build/run helpers for Windows, macOS, and Linux.

## 0.1.1 maintenance work

The current source snapshot includes the following maintenance changes:

- Keep an existing file's data extent reserved until its replacement metadata is committed; do not overwrite the old extent while preparing a replacement.
- Validate the writable filesystem's root metadata, file storage fields, parent chains, directory depth, and extent ranges during mount.
- Harden heap deallocation against out-of-range pointers, malformed allocation headers, overlapping free ranges, and common double-free corruption cases.
- Check ATA read/write command completion after the data phase rather than assuming the data transfer alone means success.
- Extract downloaded Limine/xorriso archives without following archive-provided links; reject traversal paths, special files, and duplicate case-insensitive paths.
- Add host-side tests for Python tooling, initramfs generation, and release packaging.

These changes are hardening measures, not a security audit or a guarantee of crash-safe storage.

## Build and run

### Requirements

- Python 3.10 or newer.
- Rust toolchain (`rustup` and `cargo`) from [rustup.rs](https://rustup.rs/).
- QEMU with `qemu-system-x86_64` on `PATH`.
- `xorriso` on `PATH`. The Windows build can fetch the project's xorriso fallback if needed.
- A C compiler (`cc`, Clang, or GCC) on macOS/Linux to compile the official Limine host utility when no prebuilt utility is found.
- Internet access for the first build to download the Limine binary package and, on Windows if needed, the xorriso fallback.

### Prepare the Rust target

From the repository root:

```text
python setup.py
```

### Build the ISO

```text
python build.py --clean
```

The ISO is written to `build/xavieros.iso`.

### Run in QEMU

```text
python run.py
```

`run.py` creates a 16 MiB persistent disk image at `data/xavieros-disk.img` on first run. Files written under `/DISK` remain between normal runs and rebuilds. The image is deliberately ignored by Git.

**Destructive operations:** `python run.py --reset-disk` recreates the disk image, and the in-OS command `FORMAT DISK` erases its filesystem. Use only the disposable QEMU image created by the runner; do not attach a real or valuable disk.

For troubleshooting, start with `python build.py --clean` and save the full output. QEMU's serial diagnostics are attached to the terminal.

## Testing

Run host-side checks with:

```text
python -m compileall -q build.py run.py setup.py mkinitramfs.py package_release.py tests
python -m unittest discover -s tests -v
python mkinitramfs.py --output build/initramfs-check.xfs
```

Then compare the generated image with the checked-in `initramfs.xfs` if no rootfs files were changed. Build the kernel with:

```text
cargo build --release --target x86_64-unknown-none
```

Boot QEMU and run the smoke test described in [TESTING.md](TESTING.md). Host-side tests do not start QEMU or validate physical hardware, so the kernel build and runtime checks must be run locally.

## Console command quick reference

The shell uses XavierOS's own commands, not a POSIX shell.

| Command | Purpose |
|---|---|
| `HELP`, `ABOUT`, `VERSION` | Help and kernel version |
| `SYSINFO`, `INFO`, `CPU`, `MEM`, `PMM`, `HHDM`, `HEAP` | System and memory information |
| `IDT`, `IRQ`, `TIME`, `DIAG` | Interrupt and diagnostic status |
| `LS`, `DIR`, `CAT`, `STAT`, `PWD`, `FSINFO`, `FSTEST` | Embedded and mounted filesystem access |
| `DISK`, `DINFO`, `MKDIR`, `WRITE`, `TOUCH`, `RM` | Writable `/DISK` storage |
| `FORMAT DISK` | Destructively format the writable virtual disk |
| `TASKS`, `TASKTEST` | Inspect and test cooperative tasks |
| `SELFTEST` | Run available subsystem self-tests |
| `CALC`, `ECHO`, `CLEAR`, `REBOOT`, `HALT` | Shell utilities |
| `TRAP` | Intentionally trigger a fatal invalid-opcode exception; QEMU stops |

The command set may evolve. `HELP` inside the running kernel is the authoritative list for a given build.

## Documentation

- [Architecture overview](ARCHITECTURE.md)
- [Testing and release checklist](TESTING.md)
- [Roadmap](ROADMAP.md)
- [XVRFS v1 format](XVRFS-FORMAT.md)
- [XVDISKFS v1 format](XVDISKFS-FORMAT.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)

## Status and limitations

XavierOS is experimental and is intended for learning and virtual-machine testing, not daily use or production workloads.

- Physical hardware and every BIOS/UEFI implementation have not been validated; the main test target is QEMU.
- ATA support targets a legacy PC/PIIX IDE controller through synchronous LBA28 PIO. It is not a general storage-driver stack.
- XVDISKFS is a small custom filesystem and is **not crash-safe**. Power loss or a failed write may leave inconsistent metadata; backups and recovery tools are not implemented.
- The heap, PMM, and scheduler are single-core prototypes, not an SMP-safe kernel architecture.
- Task switching is cooperative. There is no timer preemption, process isolation, user mode, ELF loader, networking, or desktop environment.
- The kernel has no general-purpose interrupt-safe locking framework. IRQ handlers must not allocate heap memory or perform disk I/O.

## Contributing

Focused bug reports and small pull requests are welcome. Include the host OS, Rust/Python/QEMU versions, full build output, serial log, and the exact smoke-test steps. Never include real disk images or private data. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

XavierOS is licensed under the MIT License. See [LICENSE](LICENSE).

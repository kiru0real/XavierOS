<div align="center">

# XavierOS

**A small x86_64 hobby operating system written in Rust.**

A freestanding `no_std` kernel with a framebuffer console, memory management, CPU exception diagnostics, interrupt-driven keyboard input, an embedded read-only initramfs, persistent virtual-disk storage, and a cooperative task scheduler prototype.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Kernel version](https://img.shields.io/badge/version-0.1.0-informational)](CHANGELOG.md)
[![Language](https://img.shields.io/badge/kernel-Rust-orange)](src/main.rs)

**XavierOS 0.1.0 — First public release**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a learning project for exploring how an operating system boots and manages memory, hardware events, files, and tasks. The kernel is written in Rust without the standard library (`#![no_std]`) and targets `x86_64-unknown-none`. Limine is used as the bootloader.

This release boots into the XavierOS text console, not a graphical desktop. It is intended for experimentation in QEMU and other disposable virtual machines, not as a daily-use or secure operating system.

## Features in 0.1.0

- Freestanding Rust kernel using the Limine boot protocol.
- Framebuffer console and built-in command shell.
- Limine memory-map parsing, Higher Half Direct Map (HHDM), and bitmap physical memory manager (PMM) for usable pages below 4 GiB.
- First-fit kernel heap backed by contiguous physical pages.
- x86_64 IDT and diagnostic handlers for CPU exceptions, including register output and CR2 reporting on page faults.
- Legacy 8259 PIC remapping and PIT timer on IRQ0; PS/2 keyboard input via IRQ1 and a bounded ring buffer.
- Serial diagnostics through COM1.
- Read-only XVRFS v1 initramfs embedded in the kernel, generated from `rootfs/` at build time.
- Legacy ATA PIO driver for the QEMU primary IDE master.
- Writable XVDISKFS v1 filesystem mounted at `/DISK`, with file and directory operations backed by a persistent 16 MiB virtual disk image.
- A small VFS dispatcher for the embedded root filesystem and writable disk mount.
- Cooperative task-context switching prototype with separate task stacks; preemptive scheduling is not implemented.
- Portable Python scripts for setup, building, and running under Windows, macOS, and Linux.

## Build and run

### Requirements

- Python 3.10 or newer.
- Rust stable toolchain with `rustup` and `cargo` ([rustup.rs](https://rustup.rs/)).
- QEMU with `qemu-system-x86_64` on `PATH`.
- `xorriso` on `PATH`; on Windows the build script can download the bundled executable on first build.
- A C compiler (`cc`, Clang, or GCC) on macOS/Linux so the Limine host utility can be compiled.
- Internet access on the first build to download the official Limine binary release and, if needed, the Windows `xorriso` bundle.

These are host-side build/run requirements. The kernel itself does not use Python at runtime.

### 1. Prepare the Rust target

From the repository root:

```text
python setup.py
```

On some systems, use `python3` instead of `python`.

### 2. Build the bootable ISO

```text
python build.py --clean
```

The script generates `build/xavieros.iso` and is configured to create a hybrid BIOS/UEFI image. The primary documented test environment is QEMU; booting on physical hardware and across all firmware implementations is not guaranteed.

### 3. Run in QEMU

```text
python run.py
```

On first launch, `run.py` creates a blank 16 MiB disk image at `data/xavieros-disk.img`. This data image persists across QEMU runs and clean builds. It is ignored by Git so that local user data is not committed.

To reset the disk (this erases all files under `/DISK`):

```text
python run.py --reset-disk
```

To boot without the writable disk attached:

```text
python run.py --no-disk
```

The current storage driver targets the primary master on a legacy PC/PIIX IDE controller in QEMU. Use only the disposable disk image created for this project; do not attach a disk with valuable data.

### Host setup notes

- **Windows:** install Rust from rustup and QEMU from the [official QEMU downloads page](https://www.qemu.org/download/#windows). The first build can download the project's `xorriso` bundle.
- **macOS:** install Rust, QEMU, and xorriso; with Homebrew: `brew install qemu xorriso`.
- **Debian/Ubuntu:** install Rust and run `sudo apt install qemu-system-x86 xorriso`.

Package names vary by operating system. Python scripts replace PowerShell-specific build tooling, but build utilities and supported hardware interfaces still have platform-specific requirements.

## Console commands

Commands are entered in uppercase ASCII. This is XavierOS's own shell, not a POSIX shell.

| Command | Description |
|---|---|
| `HELP` or `?` | List commands |
| `ABOUT`, `INFO`, `VERSION` | Project/kernel information (`0.1.0`) |
| `BANNER`, `SYSINFO`, `CPU` | Logo, system summary, and CPUID details |
| `MEM`, `PMM`, `HHDM`, `HEAP` | Memory subsystem status |
| `LS [PATH]`, `DIR [PATH]` | List a directory (defaults to `/`) |
| `CAT PATH` | Print a UTF-8 text file (maximum buffer: 4 KiB) |
| `STAT PATH` | Show file/directory metadata |
| `PWD` | Show current directory; `cd`/working-directory changes are not implemented |
| `FSINFO`, `FSTEST` | Initramfs information and checks |
| `MKDIR /DISK/PATH` | Create a directory on the writable disk |
| `WRITE /DISK/PATH TEXT` | Write text to a file on disk |
| `TOUCH /DISK/PATH` | Create an empty file without truncating an existing one |
| `RM /DISK/PATH` | Remove a file or an empty directory |
| `DISK`, `DINFO` | Show disk/filesystem status |
| `FORMAT DISK` | Destructively format the writable data disk |
| `TASKS` | List cooperative task records and switch counters |
| `TASKTEST` | Test cooperative context switching |
| `IDT` | Show IDT and exception-handler status |
| `IRQ`, `TIME` | Show interrupt/timer status and approximate uptime |
| `DIAG` | Run compact system diagnostics |
| `SELFTEST` | Run memory, interrupt, filesystem, disk-metadata and task checks |
| `TRAP` | Intentionally execute an invalid opcode; halts the guest |
| `CALC A OP B` | Checked signed integer arithmetic (`+`, `-`, `*`, `/`) |
| `ECHO TEXT`, `CLEAR` | Print text or clear the console |
| `REBOOT`, `HALT` | Reboot or halt the guest |

### Suggested smoke test

```text
HELP
IDT
IRQ
TIME
LS /
CAT /ETC/ISSUE
LS /DISK
MKDIR /DISK/HOME
WRITE /DISK/HOME/HELLO.TXT Hello from XavierOS!
CAT /DISK/HOME/HELLO.TXT
DISK
TASKTEST
TASKS
FSINFO
FSTEST
SELFTEST
DIAG
```

Restart QEMU and run `CAT /DISK/HOME/HELLO.TXT` to verify disk persistence. Run `TRAP` last because it deliberately triggers a fatal CPU exception and halts the guest.

## Project structure

```text
XavierOS/
├── .cargo/config.toml
├── src/main.rs          # Kernel entry, framebuffer, shell, keyboard decoding
├── src/memory.rs        # Memory map, HHDM, physical page allocator
├── src/interrupts.rs    # IDT, exception handlers, PIC/PIT and IRQ dispatch
├── src/heap.rs          # Kernel heap
├── src/fs.rs            # Read-only XVRFS initramfs reader
├── src/ata.rs           # Legacy ATA PIO sector I/O
├── src/diskfs.rs        # Writable XVDISKFS filesystem
├── src/vfs.rs           # VFS path and mount dispatcher
├── src/task.rs          # Cooperative task contexts and scheduler prototype
├── rootfs/              # Files embedded into initramfs
├── setup.py             # Install Rust bare-metal target
├── mkinitramfs.py       # Generate initramfs from rootfs/
├── build.py             # Compile kernel and create bootable ISO
├── run.py               # Launch QEMU and attach virtual disk
├── limine.conf
├── linker.ld
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── CHANGELOG.md
├── XVRFS-FORMAT.md
└── XVDISKFS-FORMAT.md
```

Generated `target/`, `build/`, and `tools/` directories should not be committed. Keep `Cargo.lock` under version control for this binary project. The local disk image under `data/` is deliberately ignored by Git.

## Limitations

XavierOS 0.1.0 is an experimental hobby OS. Its disk filesystem uses a simple metadata/extents layout and is not crash-safe; it must only be used with disposable virtual-disk images. ATA PIO is synchronous and currently supports the QEMU primary-master legacy IDE setup, not arbitrary storage hardware. The task scheduler is cooperative: there is no timer preemption, full blocking scheduler, user mode, or process isolation. There is no network stack or desktop. Fatal CPU exceptions print diagnostics and halt the guest. The heap is a basic first-fit allocator and has not been stress-tested. PMM manages usable physical pages below 4 GiB and is not SMP-safe. The keyboard driver assumes a conventional PS/2 controller and Set 1 scancodes; extended keys are not supported.

Run XavierOS only in QEMU or another disposable virtual machine. Do not use it on your primary machine or rely on it to protect data. Physical-hardware compatibility has not been established.

## Contributing

Bug reports and pull requests are welcome. Include your host OS, Python version, `rustc --version`, QEMU version, full build output or serial log, and reproduction steps.

## License and acknowledgements

XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Third-party tools downloaded by build scripts remain under their own licenses.

Thanks to [Rust](https://www.rust-lang.org/), [Limine](https://github.com/limine-bootloader/limine), [QEMU](https://www.qemu.org/), and [xorriso](https://www.gnu.org/software/xorriso/).

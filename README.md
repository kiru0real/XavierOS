<div align="center">

# XavierOS

**A tiny experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and tested in QEMU.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Experimental-orange)](#status-and-limitations)

**Current version: `0.1.5`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small hobby OS focused on learning kernel development: booting a machine, managing memory, handling input, storing files, and building the first layers of networking. The kernel targets `x86_64` and is written in Rust without the standard library (`#![no_std]`). The supported development target is QEMU; this is not a general-purpose operating system.

## Current features

- **Bare-metal kernel:** Limine boot protocol, framebuffer console, serial diagnostics, IDT exception handlers, PIC/PIT interrupts, and PS/2 keyboard input.
- **Memory management:** Limine memory-map parsing, physical page allocator (PMM), HHDM, kernel heap, and memory self-tests.
- **Storage:** ATA PIO for the QEMU virtual disk, writable XVDISKFS, read-only XVRFS initramfs, VFS routing, `DF`, and read-only `FSCK` checks.
- **Tasks:** a cooperative task/context-switching prototype and task diagnostics.
- **Local accounts:** experimental local users, salted PBKDF2-HMAC-SHA256 password hashes, login/logout, and basic shell-level home-directory restrictions.
- **Interactive shell:** editable command line, Up/Down history, Tab completion, and the XavierOS-branded neofetch-style `SYSINFO` screen.
- **QEMU networking:** emulated Intel E1000, Ethernet DMA rings, ARP, IPv4, ICMP `PING`, DNS A queries, DHCP client, and UDP NTP client.
- **Wall clock:** `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, and `NTP`. `SYSINFO` remains separate from wall-clock output.
- **Portable host tools:** Python build, test, run, initramfs, and release-packaging scripts for Windows, Linux, and macOS, subject to the required host tools.

## Build and run

### Requirements

- Rust toolchain installed through `rustup`.
- Python 3.10 or newer.
- QEMU with `qemu-system-x86_64` available on `PATH`.
- Host tools needed to build the Limine ISO. On macOS/Linux, install a C compiler and `xorriso`; on Windows, the setup/build scripts can prepare supported helper tools.
- Internet access may be required on the first build to download missing helper tools.

### Build and test

Run these commands from the repository root:

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The ISO is written to `build/xavieros.iso`. `run.py` starts QEMU with an emulated Intel E1000 connected through QEMU user-mode networking and attaches a persistent 16 MiB disk at `data/xavieros-disk.img`.

**Warning:** `python run.py --reset-disk` recreates the virtual disk and permanently deletes the files and local accounts stored in it. Do not use it unless you intend to erase that disk.

To prepare release assets after successfully building and testing the ISO:

```text
python package_release.py
```

The script creates `dist/xavieros-0.1.5.iso` and `dist/SHA256SUMS.txt`.

## Console commands

Commands are entered in the built-in shell; this is not a POSIX shell and it does not run Linux commands.

| Command | Description |
|---|---|
| `HELP`, `ABOUT`, `VERSION` | Help, project information, and version |
| `SYSINFO`, `INFO`, `CPU`, `MEM`, `PMM`, `HHDM`, `HEAP`, `HEALTH`, `UPTIME` | System and memory diagnostics; `SYSINFO` uses the XavierOS logo |
| `DIAG`, `SELFTEST`, `TASKS`, `TASKTEST`, `IDT`, `IRQ` | Kernel and subsystem diagnostics |
| `LS`, `CAT`, `STAT`, `PWD`, `HOME` | Browse directories, read text files, inspect entries, and show location |
| `WRITE`, `TOUCH`, `MKDIR`, `RM`, `DF`, `FSCK`, `DISK`, `FSINFO`, `FORMAT DISK` | Writable virtual-disk operations and filesystem diagnostics (`FSCK` is read-only; formatting erases disk data) |
| `USERADD`, `USERS`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD` | Experimental local account management |
| `NET`, `DHCP` | Show network status and request DHCP configuration |
| `PING 1.1.1.1 [COUNT]`, `PING example.com [COUNT]` | Send ICMP echo requests; optional count is limited to 1–8 |
| `DATE`, `TIME`, `DATETIME` | Show wall-clock date/time |
| `TIMEZONE UTC`, `TIMEZONE +03:00`, `TIMEZONE +0300` | Set a manual fixed UTC offset; compact `±HHMM` form is also supported |
| `NTP` or `NTP time.cloudflare.com` | Try to synchronize the software clock using an NTP server |
| `ECHO TEXT`, `CLEAR`, `REBOOT`, `HALT` | Basic console and VM controls |

### Network and clock notes

At startup, XavierOS attempts to obtain an IPv4 configuration with DHCP and falls back to its QEMU user-network profile when the lease cannot be acquired. Networking currently targets QEMU's emulated Intel E1000 and user-mode NAT. The clock starts from the virtual CMOS RTC (configured as UTC by the QEMU launcher) and can be adjusted by NTP. Timezone selection is a manual fixed offset; it does not infer the user's location and does not implement daylight-saving rules. The timezone and NTP synchronization are not persisted across reboot.

## Project structure

```text
XavierOS/
├── .cargo/config.toml   # Bare-metal Rust target and linker flags
├── src/
│   ├── main.rs          # Kernel entry, framebuffer console and shell
│   ├── memory.rs        # PMM, HHDM and physical-memory diagnostics
│   ├── interrupts.rs    # IDT, CPU exceptions and PIC/PIT handling
│   ├── heap.rs          # Kernel heap allocator
│   ├── task.rs          # Cooperative task/context switching
│   ├── ata.rs           # ATA PIO block I/O
│   ├── diskfs.rs        # Writable XVDISKFS
│   ├── fs.rs, vfs.rs     # Read-only initramfs and VFS layer
│   ├── users.rs         # Local user accounts and password handling
│   ├── net.rs           # E1000, IPv4, ICMP, DNS, DHCP and NTP networking
│   └── clock.rs         # RTC, wall clock and timezone conversion
├── rootfs/              # Files embedded in the read-only initramfs
├── tests/               # Host-side build-tool and regression tests
├── setup.py             # Prepare build prerequisites
├── build.py             # Compile kernel and create bootable ISO
├── run.py               # Launch QEMU with disk and E1000 networking
├── mkinitramfs.py       # Build the XVRFS initramfs image
├── package_release.py   # Produce versioned ISO and SHA-256 file
├── Cargo.toml
├── Cargo.lock
├── limine.conf
├── linker.ld
├── LICENSE
├── ARCHITECTURE.md
├── CHANGELOG.md
├── CONTRIBUTING.md
├── MIGRATION-PORTABLE.md
├── ROADMAP.md
├── SECURITY.md
├── TASKS.md
├── TESTING.md
├── XVDISKFS-FORMAT.md
├── XVRFS-FORMAT.md
└── README.md
```

Generated folders such as `target/`, `build/`, `dist/`, `tools/`, Python caches, and `data/xavieros-disk.img` should not be committed. Keep `Cargo.lock` tracked for this binary project.

## Roadmap

Likely future milestones include improving network reliability, persistent timezone/settings storage, broader filesystem recovery tools, a more complete scheduler, virtual address-space isolation, user-mode processes, and eventually a graphical desktop. These are goals, not features promised by version 0.1.5.

## Status and limitations

XavierOS is an **experimental hobby OS**, not a general-purpose or security-hardened operating system.

- Account permissions are enforced by the shell while commands still execute in ring 0. There is no full user-mode process isolation; do not use real or reused passwords.
- The network stack is experimental and targets QEMU's emulated Intel E1000 plus user-mode NAT. TCP, IPv6, network interrupts, and physical NIC support are not implemented.
- DHCP and NTP are experimental. Timezone configuration is a fixed UTC offset without automatic location detection or daylight-saving rules.
- XVDISKFS is not journaled or crash-safe. `FSCK` checks metadata but does not repair data. Keep backups of any virtual disk you care about.
- The task scheduler is cooperative. User-mode applications, isolated virtual address spaces, a desktop environment, and physical-hardware validation are not implemented.

Run XavierOS in QEMU or another disposable virtual machine. Do not rely on it to protect sensitive data.

## Contributing

Bug reports and focused pull requests are welcome. Please include the host OS, Rust and Python versions, QEMU version, full build/test output, and reproducible steps. Update `CHANGELOG.md` when a change affects users.

## License

Original XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Third-party tools and components retain their respective licenses.

## Acknowledgements

- [Rust](https://www.rust-lang.org/) and its bare-metal target support.
- [Limine](https://github.com/limine-bootloader/limine) and the [Limine boot protocol](https://github.com/Limine-Bootloader/limine-protocol).
- [QEMU](https://www.qemu.org/) for virtual-machine testing.
- [xorriso](https://www.gnu.org/software/xorriso/) for ISO image creation.

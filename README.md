<div align="center">

# XavierOS

**A tiny experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and tested primarily in QEMU.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Experimental-orange)](#status-and-limitations)

**Current version: `0.1.4` · Experimental networking release**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small hobby operating system for learning how a computer boots and how a kernel manages memory, devices, files, accounts, and network packets. The kernel is written in Rust without the standard library (`#![no_std]`) and targets `x86_64`.

XavierOS boots into a framebuffer-based interactive console. It is an experimental kernel project—not a desktop OS, a POSIX environment, or a secure multi-user system.

## Current features

- **Rust bare-metal kernel** targeting `x86_64-unknown-none`, booted through Limine.
- **Framebuffer console** with a custom pixel-style XavierOS logo and editable command line.
- **Keyboard and interrupts:** PS/2 keyboard input, PIC/PIT hardware interrupts, IDT exception handlers, and serial diagnostics.
- **Memory management:** Limine memory-map parsing, a bitmap physical memory manager (PMM), HHDM, and a small kernel heap.
- **Storage:** ATA PIO for the QEMU primary IDE disk, writable XVDISKFS, a read-only XVRFS initramfs, and a small VFS path dispatcher.
- **Cooperative task prototype** with context switching. This is not preemptive multitasking.
- **Local accounts:** password login, logout, password changes, per-user home directories, login throttling, and basic shell-level path restrictions.
- **Shell usability:** command history, Tab completion for commands and accessible paths, Left/Right, Home/End, Delete, Backspace, Ctrl+U, and Ctrl+C.
- **Filesystem tools:** `FSCK` performs a read-only consistency check; `DF` reports disk usage.
- **Neofetch-style `SYSINFO`** with the XavierOS X logo and kernel, CPU, display, memory, heap, disk, session, and scheduler information.
- **Experimental QEMU networking:** emulated Intel E1000, DMA RX/TX rings, Ethernet frame I/O, ARP, static IPv4, ICMP `PING`, and DNS A-record lookup.
- **Portable Python build scripts** for Windows, Linux, and macOS, with host-side tests.

## Build and run

### Requirements

- Rust through `rustup`.
- Python 3.10 or newer.
- QEMU with `qemu-system-x86_64` available to the run script.
- On macOS/Linux: a C compiler and `xorriso`. On Windows, the build scripts can prepare supported helper tools when needed.
- Internet access may be needed on the first build to download helper tools.

### Build the ISO

From the repository root:

```sh
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
```

The bootable ISO is written to `build/xavieros.iso`.

### Run in QEMU

```sh
python run.py
```

The run script attaches a persistent 16 MiB disk at `data/xavieros-disk.img` and configures a virtual Intel E1000 network card through QEMU user-mode networking. To erase the virtual disk—including files and user accounts—run:

```sh
python run.py --reset-disk
```

**Warning:** `--reset-disk` deletes the persistent virtual disk. Keep a backup if it contains data you need.

## First login and accounts

On a fresh virtual disk, XavierOS starts in the temporary `live` session. Create the first persistent account; it becomes the administrator:

```text
USERADD alice
LOGIN alice
WHOAMI
HOME
PASSWD
```

`USERADD` asks for the password twice and masks input. Passwords must be 8–63 printable ASCII characters and are stored as salted PBKDF2-HMAC-SHA256 hashes. After three failed login attempts, login is temporarily rate-limited. Account creation and password changes require CPU `RDRAND`; the QEMU run script uses `-cpu max` so it is typically available with QEMU TCG. Up to eight accounts are supported.

Standard users are restricted by shell-level checks to `/DISK/HOME/<name>`. Commands still execute in ring 0, so these restrictions are **not** a hardened security boundary or user-mode process isolation. Do not use real or reused passwords.

## Network quick start

The network stack currently targets QEMU's emulated Intel E1000 and user-mode NAT network. Its default profile uses `10.0.2.15/24`, gateway `10.0.2.2`, and DNS resolver `10.0.2.3`.

```text
NET
PING 10.0.2.2 1
PING 1.1.1.1 4
PING example.com 2
```

`PING <host> [count]` sends four probes by default; `count` can be from 1 to 8. Network traffic passes through QEMU and the host network, so ICMP results can depend on host policy, NAT behavior, firewalls, and the remote endpoint. QEMU may synthesize or proxy responses, so TTL does not necessarily identify the final Internet host.

DHCP, TCP, IPv6, physical NIC support, and general-purpose Internet applications are not implemented. A successful `PING` does not mean web browsing or arbitrary TCP connections are supported.

## Console commands

Commands are entered in the XavierOS shell; this is a small custom shell, not Bash or a POSIX shell.

| Area | Commands | Purpose |
|---|---|---|
| Help and identity | `HELP`, `ABOUT`, `BANNER`, `INFO`, `VERSION`, `SYSINFO` | Help, project details, version, and neofetch-style system summary |
| Hardware and diagnostics | `CPU`, `MEM`, `PMM`, `HHDM`, `HEAP`, `IRQ`, `IDT`, `DIAG`, `HEALTH`, `SELFTEST`, `UPTIME`, `TIME` | CPU, memory, interrupts, timer, and subsystem diagnostics |
| Filesystem | `DISK`, `DF`, `FSCK`, `LS`, `DIR`, `PWD`, `CAT`, `STAT`, `FSINFO`, `FSTEST`, `MKDIR`, `WRITE`, `TOUCH`, `RM` | Inspect and manage files on the supported filesystems |
| Accounts | `USERADD`, `USERS`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD`, `HOME` | Manage local accounts and sessions |
| Network | `NET`, `PING` | Inspect E1000 configuration and test ICMP/DNS |
| Tasks and utilities | `TASKS`, `TASKTEST`, `CALC`, `ECHO`, `CLEAR`, `REBOOT`, `HALT`, `TRAP` | Cooperative task prototype, calculator, console control, and exception test |

`TIME` currently reports the PIT tick counter and approximate kernel uptime; it does **not** read wall-clock time or determine a user's local timezone. `FSCK` is read-only and does not repair disk damage. `TRAP` deliberately triggers a processor exception and halts the kernel; run it only when you intend to test exception handling.

## Project structure

```text
XavierOS/
├── .cargo/                 # Bare-metal target and linker flags
├── src/                    # Kernel, memory, interrupts, storage, users, network
├── rootfs/                 # Files embedded into the read-only XVRFS image
├── tests/                  # Host-side build-tool and regression tests
├── .github/                # Issue templates
├── Cargo.toml / Cargo.lock
├── limine.conf / linker.ld
├── setup.py / build.py / run.py
├── mkinitramfs.py / package_release.py
├── README.md / README.ru.md
├── CHANGELOG.md
└── LICENSE
```

Generated folders such as `target/`, `build/`, and `tools/` are not source files and should not be committed. The persistent disk image under `data/` is local state and should stay out of Git.

## Documentation

The repository contains focused notes for the parts of the project that need more detail:

- [Architecture](ARCHITECTURE.md) — boot flow and subsystem boundaries.
- [Changelog](CHANGELOG.md) — changes by version.
- [Testing](TESTING.md) — host tests and QEMU verification checklist.
- [XVRFS format](XVRFS-FORMAT.md) — read-only initramfs image format.
- [XVDISKFS format](XVDISKFS-FORMAT.md) — writable virtual-disk filesystem format.
- [Security policy](SECURITY.md) — current security scope and reporting issues.
- [Contributing](CONTRIBUTING.md) — how to suggest and submit changes.
- [Roadmap](ROADMAP.md) — planned milestones, not guaranteed features.
- [Task subsystem notes](TASKS.md) — current cooperative scheduler prototype.
- [Portable build notes](MIGRATION-PORTABLE.md) — host-specific build requirements.
- [MIT license](LICENSE).

## Status and limitations

XavierOS is an **experimental hobby OS**. The supported development target is QEMU; physical hardware has not been validated. The kernel has no desktop environment, user-mode process isolation, preemptive scheduler, virtual address-space isolation, TCP, IPv6, DHCP, or broad device support. XVDISKFS is not journaled or crash-safe. The account manager and filesystem checks are prototypes, not security guarantees.

Run XavierOS in a disposable virtual machine and keep backups of virtual disks. Do not rely on it for sensitive data or production workloads.

## Contributing

Bug reports and focused contributions are welcome. Include your host OS, Rust and QEMU versions, the full build output or serial log, and exact reproduction steps. Please keep documentation links accurate and update `CHANGELOG.md` when user-visible behavior changes.

## License

Original XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Third-party tools such as Limine and `xorriso` remain under their respective licenses.

## Acknowledgements

- [Rust](https://www.rust-lang.org/) and its bare-metal target support.
- [Limine](https://github.com/limine-bootloader/limine) and the [Limine boot protocol](https://github.com/Limine-Bootloader/limine-protocol).
- [QEMU](https://www.qemu.org/) for virtual-machine testing.
- [xorriso](https://www.gnu.org/software/xorriso/) for ISO image creation.

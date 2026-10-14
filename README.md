# XavierOS

**Version 0.1.2** · Experimental x86_64 hobby operating system written in Rust.  
[Русская версия README](README.ru.md)

XavierOS is a small `no_std` kernel booted by Limine. It is built for learning and QEMU experiments, not everyday or production use.

## Table of contents

- [Features](#features)
- [Build and run](#build-and-run)
- [Accounts](#accounts)
- [Shell](#shell)
- [Documentation](#documentation)
- [Limitations and safety](#limitations-and-safety)
- [License](#license)

## Features

- Limine boot, framebuffer console, PS/2 keyboard, PIC/PIT interrupts and IDT exception diagnostics.
- Physical page allocator, HHDM and kernel heap.
- ATA PIO block I/O and writable XVDISKFS on a persistent QEMU disk.
- Read-only XVRFS initramfs and a small VFS mount dispatcher.
- Cooperative task/context-switch prototype.
- Local account database, password login, home directories and basic shell-level path restrictions.
- 16-entry command history with Up/Down and Tab completion for commands and accessible VFS paths.
- Portable Python build/run scripts for Windows, macOS and Linux.

## Build and run

Requirements: Rust via `rustup`, Python 3.10+, QEMU and the host tools described below. On macOS/Linux, install a C compiler and `xorriso`; on Windows the setup script can prepare supported helper tools.

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The build creates `build/xavieros.iso`. `run.py` attaches a persistent 16 MiB disk at `data/xavieros-disk.img`. To erase and recreate that disk (this deletes files and accounts), run `python run.py --reset-disk`.

## Accounts

XavierOS starts in a temporary `live` session. On a fresh disk, create the first account; it becomes the administrator:

```text
USERADD alice
LOGIN alice
WHOAMI
HOME
PASSWD
```

`USERADD` asks for a password twice and masks the input. Passwords must contain 8–63 printable ASCII characters, are case-sensitive, and are stored as salted PBKDF2-HMAC-SHA256 hashes. Up to eight persistent accounts are supported. The first persistent account is the administrator. An administrator can create additional accounts with `USERADD bob`; standard users can access the writable disk only through their own `/DISK/HOME/<name>` directory. `LOGOUT` returns to the temporary `live` session, which becomes restricted after the first account exists. `USERS` lists accounts for the administrator.

## Shell

Press **Up/Down** to browse recent commands. Press **Tab** to complete a command (for example, `ho` → `HOME`) or a unique accessible file/directory path. Ambiguous matches may extend to their common prefix; if the prefix does not grow, the line is left unchanged.

Useful commands: `HELP`, `SYSINFO`, `LS`, `CAT`, `WRITE`, `MKDIR`, `RM`, `DISK`, `TASKS`, `SELFTEST`, `WHOAMI`, `LOGIN`, `LOGOUT`, `PASSWD`.

## Documentation

The README is the starting point. These focused documents explain the design, formats, testing and contribution workflow:

| Document | Purpose |
|---|---|
| [`CHANGELOG.md`](CHANGELOG.md) | Changes by version |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Kernel components and boot/data flow |
| [`TASKS.md`](TASKS.md) | Cooperative task prototype and limitations |
| [`ROADMAP.md`](ROADMAP.md) | Planned development direction |
| [`TESTING.md`](TESTING.md) | Host-side, QEMU, account and disk regression checks |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Guidelines for contributions |
| [`SECURITY.md`](SECURITY.md) | Security limitations and reporting guidance |
| [`XVRFS-FORMAT.md`](XVRFS-FORMAT.md) | Read-only initramfs image format |
| [`XVDISKFS-FORMAT.md`](XVDISKFS-FORMAT.md) | Writable virtual-disk filesystem layout |
| [`MIGRATION-PORTABLE.md`](MIGRATION-PORTABLE.md) | Portable build tools and GitHub browser upload notes |

## Limitations and safety

- The account manager is experimental. Commands run in ring 0; this is **not** user-mode process isolation or a hardened security boundary. Do not use real or reused passwords.
- Salt generation prefers CPU RDRAND when available and falls back to a timer/TSC-derived value on CPUs that do not expose it. The fallback is not a cryptographic random-number generator.
- XVDISKFS is small and is not journaled or crash-safe. Use disposable virtual disk images.
- The scheduler is cooperative. User-mode programs, virtual address-space isolation, networking, a desktop and broad physical-hardware support are not implemented.
- QEMU is the supported development target; physical hardware is not validated.

`FORMAT DISK` erases the writable filesystem, including user accounts. The persistent disk image is local state and is excluded from Git.

## License

MIT. See [`LICENSE`](LICENSE).

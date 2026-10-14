# XavierOS

**Version 0.1.3** · Experimental x86_64 hobby operating system written in Rust.

XavierOS is a small `no_std` kernel booted by Limine. It is built for learning and QEMU experiments, not everyday or production use.

## Features

- Limine boot, framebuffer shell, PS/2 keyboard and PIC/PIT interrupts.
- IDT exception handlers with serial diagnostics; PMM, HHDM and a kernel heap.
- ATA PIO, writable XVDISKFS on a persistent QEMU disk, read-only XVRFS initramfs and VFS.
- Cooperative task/context-switch prototype.
- Local user accounts, PBKDF2-HMAC-SHA256 password hashes, login throttling and basic shell-level path restrictions.
- Command history, Tab completion, editable input line, `FSCK`, `DF`, `HEALTH` and neofetch-style `SYSINFO`.

## Build and run

Requirements: Rust via `rustup`, Python 3.10+, QEMU, and host tools. On macOS/Linux install a C compiler and `xorriso`; on Windows the build script can prepare its supported helper tools.

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The build creates `build/xavieros.iso`. `run.py` attaches a persistent 16 MiB disk at `data/xavieros-disk.img`. To erase that disk and all accounts/files, run `python run.py --reset-disk`.

The shell uses a thin vertical input caret, command history, and Tab completion.

## Accounts

On a fresh disk, the temporary `live` session can create the first account, which becomes administrator:

```text
USERADD alice
LOGIN alice
WHOAMI
HOME
PASSWD
```

Passwords are hidden while typing, must be 8–63 printable ASCII characters, are case-sensitive, and are stored as salted PBKDF2-HMAC-SHA256 hashes. After three failed sign-in attempts, login is rate-limited for five seconds. Account creation/password changes require CPU RDRAND; `run.py` uses QEMU `-cpu max` so this is typically available under QEMU TCG. Accounts created by 0.1.2 keep their existing salt until the user runs `PASSWD` once. Up to eight accounts are supported. Standard users are restricted by shell checks to their own `/DISK/HOME/<name>` directory.

## Shell quick reference

- **Editing:** Left/Right, Home/End, Delete, Backspace, Ctrl+U (clear line), Ctrl+C (cancel input).
- **History and completion:** Up/Down browse 16 recent commands; Tab completes command names and accessible paths.
- **Disk:** `DISK`, `DF`, `FSCK` (read-only metadata check), `LS`, `CAT`, `WRITE`, `MKDIR`, `RM`.
- **System:** `SYSINFO`, `HEALTH`, `UPTIME`, `MEM`, `HEAP`, `PMM`, `IRQ`, `TASKS`, `SELFTEST`.
- **Accounts:** `USERADD`, `USERS`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD`, `HOME`.

`SYSINFO` deliberately keeps a neofetch-like layout: X logo, OS/kernel, CPU, display, user/session, uptime, RAM/PMM, heap, disk and scheduler. `HEALTH` is a compact subsystem status report, not a security audit. `FSCK` checks the on-disk superblock, metadata, directory tree, extents and overlap without writing repairs.

## Limitations

- Account management and access checks are experimental; commands run in ring 0. This is not user-mode process isolation or a hardened security boundary. Do not use real or reused passwords.
- Password salt generation is fail-closed: without RDRAND, account creation/password changes are disabled rather than using a predictable timer/TSC fallback.
- XVDISKFS is not journaled or crash-safe. FSCK is read-only and does not repair corruption. Use disposable virtual disk images.
- Scheduling is cooperative. User-mode processes, address-space isolation, networking, a desktop and broad physical-hardware support are not implemented.
- QEMU is the supported development target; physical hardware is not validated.

## Documentation

- [Russian README](README.ru.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE)

MIT license.

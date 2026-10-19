<div align="center">

# XavierOS

**A tiny experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and tested in QEMU.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Experimental-orange)](#status-and-limitations)

**Current version: `0.1.8` · Development target: `v0.1.8`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small hobby OS for learning kernel development. Its kernel is written in Rust without the standard library (`#![no_std]`) and targets `x86_64`. It boots through Limine and is intended for disposable virtual-machine experiments, not everyday or production use.

## Current features

- Limine boot, framebuffer console, PS/2 keyboard, IDT exceptions, PIC/PIT interrupts, and serial diagnostics.
- Limine memory-map parsing, bitmap PMM, HHDM, kernel heap, and a cooperative task-switching prototype.
- ATA PIO virtual disk, writable XVDISKFS, read-only XVRFS initramfs, and a VFS dispatcher.
- Experimental local accounts with salted PBKDF2-HMAC-SHA256 password hashes and basic shell-level home-directory restrictions.
- Editable command line, Up/Down history, Tab completion, and neofetch-style `SYSINFO` with the XavierOS X logo.
- QEMU E1000 networking: DMA rings, ARP, IPv4, ICMP `PING`, DNS A queries, a DHCP client, and UDP NTP client.
- Experimental synchronous TCP/HTTP client with bounded retries, redirects, `Content-Length`, and chunked response decoding.
- `HTTPGET` for displaying a bounded response and `FETCH` for saving a completed small HTTP body to disk.
- `NETSTAT` and `NETCHECK` for network counters and basic connectivity checks.
- `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, and `NTP` commands.
- Checksummed persistent configuration with alternating slots, `CONFIG`/`SETTINGS`, and a `REPORT` diagnostic summary.
- Safer `FETCH` commit: write to a temporary sibling file, then rename after the full response has been received.

## Build and run

### Requirements

- Windows 10/11, Linux, or macOS.
- Rust via `rustup`, including the `x86_64-unknown-none` target.
- Python 3.10+ and QEMU (`qemu-system-x86_64`).
- On Linux/macOS, a C compiler and `xorriso`; on Windows, supported helper tools can be prepared by the build script.
- Internet access during the first build if required helper tools are missing.

Run from the repository directory:

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The build creates `build/xavieros.iso`. The QEMU launcher attaches a persistent 16 MiB disk at `data/xavieros-disk.img`, enables the emulated Intel E1000 NIC through user-mode NAT, and sets the virtual RTC base to UTC. **`python run.py --reset-disk` erases files, user accounts, and saved configuration.**

## Console commands

XavierOS has a small built-in shell; it is not a POSIX shell and does not run Linux commands.

| Command | Description |
|---|---|
| `HELP`, `ABOUT`, `VERSION`, `BANNER` | Help, project information, version, and logo |
| `SYSINFO`, `INFO`, `CPU`, `MEM`, `PMM`, `HHDM`, `HEAP` | System and memory information |
| `HEALTH`, `DIAG`, `SELFTEST`, `IDT`, `IRQ`, `UPTIME`, `REPORT` | Diagnostics and system report |
| `LS [PATH]`, `CAT PATH`, `STAT PATH`, `PWD`, `FSINFO`, `FSTEST` | Browse and inspect files |
| `WRITE PATH TEXT`, `TOUCH PATH`, `MKDIR PATH`, `RM PATH`, `DF`, `FSCK`, `DISK` | Writable disk and filesystem tools (`FSCK` is a read-only metadata check; it does not repair data) |
| `USERADD`, `LOGIN`, `LOGOUT`, `WHOAMI`, `USERS`, `PASSWD`, `HOME` | Experimental local accounts |
| `CONFIG`, `SETTINGS` | Show persistent system settings |
| `CONFIG SET TIMEZONE +0300` | Change the manual timezone offset |
| `CONFIG SET NTP time.cloudflare.com` | Set the preferred NTP server |
| `CONFIG SET NETWORK DHCP` / `STATIC` | Choose DHCP or the QEMU static profile |
| `CONFIG SET TIMEOUT 10` | Set network timeout in seconds (1–120) |
| `CONFIG SAVE`, `CONFIG RESET` | Save configuration or restore defaults |
| `REPORT SAVE /DISK/HOME/USER/REPORT.TXT` | Save a diagnostic report to an accessible disk path |
| `NET`, `DHCP`, `NETSTAT`, `NETCHECK` | Network setup and diagnostics |
| `PING HOST [COUNT]` | ICMP echo test, 1–8 probes |
| `HTTPGET http://HOST/PATH` | Display a bounded plain-HTTP response |
| `FETCH http://HOST/PATH /DISK/PATH` | Save a completed small plain-HTTP response body |
| `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, `NTP [SERVER]` | Wall-clock display and NTP synchronization |

### Examples

```text
CONFIG
CONFIG SET TIMEZONE +0300
CONFIG SET NTP time.cloudflare.com
CONFIG SET NETWORK DHCP
CONFIG SET TIMEOUT 10
CONFIG SAVE
NETCHECK
PING example.com 2
HTTPGET http://neverssl.com/
FETCH http://example.com/ /DISK/HOME/alice/example.html
REPORT
REPORT SAVE /DISK/HOME/alice/REPORT.TXT
```

Replace `alice` in examples with your own persistent username. Only the first persistent account is the administrator. Configuration changes require administrator access. The account system is experimental; commands still run in ring 0, so this is not a complete isolation boundary.

`FETCH` accepts only plain HTTP, writes under `/DISK`, checks user path permissions, rejects an existing destination, and saves the body to a temporary sibling file before committing it by rename. Its response buffer is limited to 16 KiB. Downloaded content is untrusted.

## Update notifications

Automatic GitHub release checking is not implemented yet. HTTPS/TLS support is required because the GitHub Releases API requires HTTPS, while XavierOS does not implement HTTPS/TLS. The OS will not send an insecure plain-HTTP request for update metadata. Once TLS with certificate validation exists, a startup check can compare the installed version with the latest published release and show a non-blocking notification without downloading or installing anything automatically.

## Status and limitations

- XavierOS is experimental and targets QEMU's emulated Intel E1000. Physical hardware is not validated.
- User accounts do not provide a full security boundary because there is no user-mode isolation; do not reuse real passwords.
- TCP is an experimental synchronous HTTP client, not a complete TCP stack. Congestion control, persistent connections, robust recovery after partial-data loss, HTTPS/TLS, IPv6, and physical NIC support are not implemented.
- DHCP lease renewal, automatic timezone/location detection, daylight-saving rules, and persistence of the synchronized wall-clock baseline are not implemented. `TIMEZONE` lets you set a manual fixed UTC offset. The timezone offset, NTP server, network mode, and timeout can be saved in the persistent config.
- XVDISKFS is not journaled or crash-safe. Alternating checksummed configuration slots reduce the chance of losing the previous config, but they do not make the filesystem generally crash-safe. `FSCK` only checks; it does not repair.
- The scheduler is cooperative. Desktop, user-mode ELF loader, and virtual-address-space isolation are not implemented.

Use disposable virtual disks and keep backups. The local disk image `data/xavieros-disk.img` is runtime state and must not be committed.

## Project structure

```text
XavierOS/
├── .cargo/             # Bare-metal target and linker configuration
├── src/                # Kernel, memory, storage, accounts, clock, config, network
├── tests/              # Host-side tests and source regression checks
├── rootfs/             # Read-only XVRFS initramfs contents
├── Cargo.toml
├── build.py            # Build kernel and bootable ISO
├── run.py              # Launch QEMU with persistent disk and E1000 NAT
├── mkinitramfs.py      # Build XVRFS initramfs
├── package_release.py  # Package versioned ISO and checksum file
├── CHANGELOG.md
├── LICENSE
├── README.md
└── README.ru.md
```

Generated `target/`, `build/`, `tools/`, `dist/`, Python caches, and the persistent disk image are not source code and should not be committed.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for version history.

## License

Original XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Third-party tools and components retain their own licenses.

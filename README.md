<div align="center">

# XavierOS

**A tiny experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and tested in QEMU.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Experimental-orange)](#status-and-limitations)

**Current version: `0.1.6` · Development release target: `v0.1.6`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small hobby operating system focused on learning kernel development. The kernel is written in Rust without the standard library (`#![no_std]`) and targets `x86_64`. It boots through Limine and is intended for virtual-machine experiments, not everyday or production use.

## Current features

- Limine boot, framebuffer console, PS/2 keyboard, IDT exception handlers, PIC/PIT interrupts, and serial diagnostics.
- Limine memory map, bitmap physical memory manager (PMM), HHDM, kernel heap, and cooperative task-switching prototype.
- ATA PIO virtual disk, writable XVDISKFS, read-only XVRFS initramfs, and a small VFS layer.
- Experimental local accounts with salted PBKDF2-HMAC-SHA256 password hashes and basic shell-level home-directory restrictions.
- Editable command line, Up/Down history, Tab completion, and a neofetch-style `SYSINFO` with the XavierOS X logo.
- QEMU Intel E1000 networking: DMA rings, ARP, IPv4, ICMP `PING`, DNS A queries, a DHCP client, and a UDP NTP client.
- Experimental one-shot TCP client for plain HTTP/1.0 requests via `HTTPGET`.
- Network diagnostics through `NET`, `NETSTAT`, and `NETCHECK`, plus transmit/receive frame counters.
- Wall-clock commands `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, and `NTP`.

## Build and run

### Requirements

- Windows 10/11, Linux, or macOS.
- Rust via `rustup`, including the `x86_64-unknown-none` target.
- Python 3.10 or newer.
- QEMU (`qemu-system-x86_64`).
- A C compiler and `xorriso` on Linux/macOS; the build script can prepare supported helper tools on Windows.
- Internet access for the first build if the Limine and Windows `xorriso` helper tools are not present.

### Commands

Run from the repository directory:

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The build creates `build/xavieros.iso`. The QEMU launcher attaches a persistent 16 MiB disk at `data/xavieros-disk.img`, enables the emulated Intel E1000 NIC through user-mode NAT, and sets the virtual RTC base to UTC. **Do not use `python run.py --reset-disk` unless you want to erase all files and user accounts.**

## Console commands

Commands are entered in the built-in XavierOS shell. It is not a POSIX shell and does not run Linux commands.

| Command | Description |
|---|---|
| `HELP`, `ABOUT`, `VERSION`, `BANNER` | Help, project information, version, and logo |
| `SYSINFO`, `INFO`, `CPU`, `MEM`, `PMM`, `HHDM`, `HEAP` | System and memory summary |
| `HEALTH`, `DIAG`, `SELFTEST`, `IDT`, `IRQ`, `UPTIME` | Kernel diagnostics and tests |
| `LS [PATH]`, `CAT PATH`, `STAT PATH`, `PWD`, `FSINFO`, `FSTEST` | Browse and inspect files |
| `WRITE PATH TEXT`, `TOUCH PATH`, `MKDIR PATH`, `RM PATH`, `DF`, `FSCK`, `DISK` | Writable disk and filesystem tools (`FSCK` is a read-only metadata check; it does not repair data) |
| `USERADD`, `LOGIN`, `LOGOUT`, `WHOAMI`, `USERS`, `PASSWD`, `HOME` | Experimental local accounts |
| `NET`, `DHCP` | Network status and IPv4 configuration |
| `PING HOST [COUNT]` | ICMP echo test; count range is 1–8 (default 4) |
| `NETSTAT` | Show IP configuration and E1000 frame/error counters |
| `NETCHECK` | Check E1000, link, gateway ARP, and a DNS lookup for `example.com` |
| `HTTPGET http://HOST/PATH` | Fetch a small plain-HTTP resource and display its status, headers, and body |
| `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, `NTP [SERVER]` | Wall-clock display, manual UTC offset, and NTP synchronization |
| `CALC A OP B`, `ECHO TEXT`, `CLEAR`, `REBOOT`, `HALT` | Calculator, output, console clear, reboot, and halt |

### Network examples

```text
NET
NETCHECK
NETSTAT
PING 10.0.2.2 1
PING example.com 4
HTTPGET http://neverssl.com/
NTP time.cloudflare.com
DATETIME
TIMEZONE +0300
```

`HTTPGET` currently supports a single synchronous TCP connection using plain HTTP. HTTPS/TLS, redirects, chunked-transfer decoding, large downloads, TCP connection reuse, and general TCP applications are not implemented. The response buffer is bounded; binary response bytes are displayed as dots rather than executed or interpreted as terminal control sequences.

## Important limitations

- XavierOS is experimental. Run it in QEMU or another disposable virtual machine; it has not been validated for general-purpose physical hardware.
- User accounts do not provide a full security boundary: shell commands still run in ring 0, and user-mode process isolation is not implemented. Do not reuse real passwords.
- Networking targets QEMU's emulated Intel E1000 and user-mode NAT. DHCP, DNS A, ICMP, NTP, and a basic HTTP/TCP path are experimental. IPv6, TLS/HTTPS, TCP retransmission/congestion control, physical NICs, and E1000 interrupt mode are not implemented.
- The TCP client is minimal and synchronous. It uses bounded buffers and timeouts, but is not a complete general-purpose TCP stack.
- NTP and timezone settings are volatile across reboot. `TIMEZONE` lets you set a manual fixed UTC offset; automatic location detection and daylight-saving rules are not implemented.
- XVDISKFS is not journaled or crash-safe. `FSCK` checks metadata and does not repair files or recover interrupted writes. Keep backups of disposable test disks.
- The scheduler is cooperative. A desktop, userspace ELF loader, and virtual-address-space isolation are not implemented.

## Project structure

```text
XavierOS/
├── .cargo/             # Bare-metal target and linker configuration
├── src/                # Kernel, memory, storage, users, clock, and network modules
├── tests/              # Host-side build-tool and regression tests
├── rootfs/             # Files packed into the read-only XVRFS initramfs
├── Cargo.toml
├── build.py            # Build kernel and bootable ISO
├── run.py              # Launch QEMU with persistent disk and E1000 NAT
├── mkinitramfs.py      # Create the XVRFS initramfs image
├── package_release.py  # Package versioned ISO and SHA-256 file
├── CHANGELOG.md
├── LICENSE
├── README.md
└── README.ru.md
```

Generated output such as `target/`, `build/`, `tools/`, `dist/`, and the local persistent disk image is not source code and should not be committed.

## Roadmap

Possible future work includes more complete TCP behavior and HTTPS/TLS, persistent system configuration, stronger process isolation, interrupt-driven networking, a userspace program loader, and eventually a graphical desktop. These are plans, not features promised by version `0.1.6`.

## License

Original XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Third-party tools and components remain under their own licenses.

<div align="center">

# XavierOS

**An experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and tested in QEMU.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Desktop%20Preview%20Beta-orange)](#status-and-limitations)

**Current version: `0.2.0-beta.4` · Smoother desktop and windowed Terminal(available as pre-release)**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small `no_std` Rust kernel for learning low-level systems programming. It boots through Limine, draws directly to the framebuffer, handles PS/2 input, manages physical pages and a kernel heap, and includes an experimental storage and networking stack. Version `0.2.0-beta.4` smooths wallpaper scaling with bilinear interpolation, gives Files, Network, Clock, Settings and About a consistent app design, and runs the existing interactive shell inside a Terminal window. Terminal supports pointer movement and can be closed with Escape or its title-bar close control.

## What's included

- **Bare-metal Rust kernel** for `x86_64-unknown-none`, booted with Limine.
- **Framebuffer UI** with a custom bitmap font, a full-screen cover-scaled scenic wallpaper, a top menu bar, and a redesigned glass-style app dock with custom vector icons and a custom mouse pointer sourced from a transparent 64×64 PNG and rendered at 28×28 pixels with alpha blending and a contrast halo.
- **Mouse and keyboard interaction:** polled PS/2 mouse when available, keyboard dock navigation as a fallback, selectable app icons, Escape to close preview windows, and a windowed Terminal that opens the existing shell.
- **Preview apps:** Files lists VFS entries, Network shows the E1000/IPv4 configuration, Clock displays the wall clock, Settings provides an interactive display-resolution panel, and About describes the build. These app windows are early in-kernel preview panels, not independent user processes.
- **Interactive shell:** command history, line editing, Up/Down history, Tab completion, user accounts, filesystem tools, diagnostics, and the neofetch-style `SYSINFO` screen. The shell keeps draining the polled mouse stream so keyboard input is not blocked while Terminal is open. Press `Escape` or click the red title-bar control to close Terminal, or type `DESKTOP` to return to the desktop.
- **Memory management:** Limine memory map, PMM, HHDM, heap, IDT exception reporting, PIC/PIT timer and keyboard IRQ.
- **Storage:** ATA PIO, read-only XVRFS initramfs, VFS dispatcher and writable XVDISKFS on a persistent QEMU disk.
- **Experimental networking:** QEMU E1000, DHCP client with fallback, ARP/IPv4, ICMP `PING`, DNS/NTP, and a bounded HTTP client.
- **Portable Python build scripts** for Windows, Linux and macOS hosts, with QEMU as the main test environment.

## Build and run

### Requirements

- Rust via `rustup` and Python 3.10+.
- QEMU with `qemu-system-x86_64` available on `PATH`.
- On Windows, the build script can prepare supported helper tools. On macOS/Linux, install a C compiler and `xorriso`.

```powershell
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The build produces `build/xavieros.iso`. `run.py` attaches a persistent 16 MiB disk at `data/xavieros-disk.img`; **do not use `--reset-disk` unless you want to erase files, accounts and settings**.

## Desktop preview controls

- **Mouse:** move the pointer and click a dock icon, if the PS/2 auxiliary device is available.
- **Keyboard:** Left/Right to select a dock icon, Enter to open it, Escape to close an app preview, and `T` to open Terminal. `F`, `N`, `C`, `S`, and `A` open Files, Network, Clock, Settings, and About.
- **Terminal:** the existing shell runs inside the Terminal window. Press `Escape`, click the red title-bar control, or type `DESKTOP` to return to the desktop. All existing shell commands remain available.

The desktop is intentionally a preview: Terminal is the real shell rendered inside an app window; the other apps are framebuffer panels backed by existing kernel subsystems. Terminal currently takes keyboard focus while its shell is active, so other apps cannot be focused until Terminal closes. Keyboard shortcuts supplement dock navigation: `T`, `F`, `N`, `C`, `S`, and `A` open Terminal, Files, Network, Clock, Settings, and About.

### Display resolution

Limine still offers `1024x768`, `1280x720`, and `1366x768` at startup. Settings now lets you select a mode card and choose Apply Mode or Use Current. The selected mode applies to the current QEMU session; the Limine boot selection remains the startup default after a restart. `run.py` explicitly starts QEMU with standard VGA; when the Bochs VBE interface is present and enough video memory is available, Apply Mode switches resolution immediately and updates the framebuffer layout. If live switching is not supported, Settings displays an error and the Limine startup menu remains the fallback. The scenic wallpaper uses bilinear cover scaling with a centered crop to reduce pixelation while filling the display without distortion or letterboxing.


## Console commands

Commands are currently entered as ASCII text. Use `HELP` for the built-in list. Common commands include:

| Command | Description |
|---|---|
| `SYSINFO` | XavierOS-branded system summary in a neofetch-like layout |
| `NET`, `NETSTAT`, `NETCHECK`, `PING` | Network status and diagnostics |
| `HTTPGET`, `FETCH` | Experimental plain-HTTP client and file download |
| `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, `NTP` | Wall-clock and NTP tools |
| `CONFIG`, `SETTINGS`, `REPORT` | Persistent config and diagnostic report |
| `LS`, `CAT`, `STAT`, `WRITE`, `MKDIR`, `RM`, `DF`, `FSCK` | Filesystem operations and checks |
| `USERADD`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD` | Experimental local account management |
| `HEALTH`, `SELFTEST`, `TASKS`, `HEAP`, `MEM` | Kernel and subsystem diagnostics |
| `DESKTOP` | Return from Terminal to Desktop Preview |

## Configuration and filesystem notes

- `CONFIG SET TIMEZONE` sets a manual fixed UTC offset (for example `TIMEZONE +0300`); automatic location detection and daylight-saving rules are not implemented.
- `CONFIG SET NETWORK` selects DHCP or the static QEMU fallback. `CONFIG SAVE` persists settings on the virtual disk.
- `REPORT SAVE` writes a diagnostic snapshot to an accessible path under `/DISK`.
- `FSCK` is a read-only metadata check: it scans filesystem structures but does not repair the disk. Back up the virtual disk before experimenting with low-level storage tools.
- The clock uses the CMOS RTC until NTP sync is successful. The UDP NTP client is experimental.
- Automatic GitHub release checking is not implemented yet. HTTPS/TLS support is required before the OS can safely query the GitHub Releases API.

## Project structure

```text
XavierOS/
├── .cargo/config.toml
├── src/                 # Kernel, memory, storage, network, clock and desktop modules
├── assets/              # Source wallpaper and embedded RGB bitmap
├── tests/               # Host-side build/tooling regression tests
├── rootfs/              # Read-only initramfs content
├── setup.py
├── build.py
├── run.py
├── package_release.py
├── mkinitramfs.py
├── Cargo.toml / Cargo.lock
├── limine.conf / linker.ld
├── README.md / README.ru.md / CHANGELOG.md
└── LICENSE
```

## Documentation

- [Changelog](CHANGELOG.md)
- [License](LICENSE)

The repository keeps its top-level documentation intentionally compact. Build and desktop-preview instructions are maintained in this README.

## Status and limitations

XavierOS is an **experimental hobby OS**, not a general-purpose or secure operating system. The desktop preview runs in kernel space; preview apps are not isolated user processes. User accounts enforce shell-level access rules but do not provide a ring-3 security boundary. XVDISKFS is not journaled or crash-safe. Networking currently targets QEMU's emulated E1000 and user-mode NAT; HTTPS/TLS, IPv6, and general-purpose networking are not implemented. The PS/2 mouse is polled and keyboard navigation remains the fallback. Physical-hardware support is not validated.

**Run XavierOS in QEMU or another disposable virtual machine. Do not rely on it to protect important data.**

## License

XavierOS source code and documentation are distributed under the [MIT License](LICENSE). Downloaded third-party tools and components remain under their respective licenses.

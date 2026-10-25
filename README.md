<div align="center">

# XavierOS

**A hobby x86_64 operating system written in Rust.**

A `no_std` kernel booting with Limine, with a framebuffer desktop, a shell, storage, and experimental networking.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-0.2.0%20Release-blue)](#status-and-limitations)

**Current version: `0.2.0`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a learning-oriented hobby operating system written in Rust without the standard library. It boots through Limine, renders directly to a framebuffer, handles legacy PS/2 input, manages physical memory and a kernel heap, and includes experimental virtual-disk and networking subsystems. Release 0.2.0 adds navigable Files, faster window redraws, smoother wallpaper scaling, a compact custom cursor, display settings, and a top-left XavierOS system menu.

## Features

- **Kernel and boot:** `x86_64-unknown-none`, Limine boot protocol, IDT exception handlers, PMM bitmap allocator, HHDM, heap, PIC/PIT, PS/2 keyboard and polled PS/2 mouse.
- **Desktop compositor:** fullscreen wallpaper with cover scaling and fixed-point bilinear interpolation, updated dock and icons, a 24×24 custom alpha-blended cursor, a top menu bar, window panels, and saved-window-background restoration to avoid the old full-screen “printer” redraw on close.
- **Files:** click directories to navigate, use Up/Home/Disk/System, page through entries, select and preview a text file, create a directory or empty file, and delete a selected file on the writable `/DISK` mount. The embedded root is read-only; preview is bounded and not a full text editor.
- **Terminal:** existing kernel shell rendered inside a window, with line editing, command history, path/command Tab completion, account commands, filesystem commands, network diagnostics, and system reports. The Terminal takes keyboard focus while running.
- **Network:** E1000 for QEMU, DHCP/static fallback, ARP/IPv4, ICMP `PING`, DNS/NTP, experimental TCP/HTTP, gateway/DNS diagnostics, and packet counters.
- **Clock:** local time, fixed UTC offset, RTC and experimental NTP synchronization controls.
- **Settings:** three display profiles—1024×768, 1280×720, 1366×768—with a guarded Bochs VBE live-mode request when supported. Limine profiles remain the boot-time fallback.
- **About:** overview, kernel, and license tabs.
- **Storage:** read-only XVRFS initramfs plus writable XVDISKFS over a QEMU ATA PIO virtual disk.
- **Build tooling:** Python scripts for initramfs creation, building, launching and release packaging; host-side regression tests.

## Build and run

Requirements: Rust via `rustup`, Python 3.10+, QEMU (`qemu-system-x86_64`), and the boot-image prerequisites described in the build script (including `xorriso`/Limine tooling where needed).

```powershell
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The ISO is written under `build/`. `run.py` attaches `data/xavieros-disk.img`, a persistent virtual disk; back it up before experimenting. Do not use `--reset-disk` unless you intentionally want to recreate the virtual disk.

## Desktop controls

- Mouse: click Dock icons and app controls when a PS/2 auxiliary device is available.
- Keyboard: `F` Files, `T` Terminal, `N` Network, `C` Clock, `S` Settings, `A` About, `X` system menu. Arrow keys navigate the menu, Files list, and About tabs; Enter activates; Escape closes or moves up in Files. In Network, `G` checks the gateway and `D` checks DNS. In Clock, `N` syncs NTP, `M`/`P` adjust timezone by one hour, and `S` saves it.
- Files: click a folder to enter it; use `UP`/Escape to go to the parent, `PREV`/`NEXT` to page, select a file to preview, `NEW DIR` or `NEW FILE` to create an item under `/DISK`, and `DELETE` to remove a selected `/DISK` file. During name entry, type ASCII and press Enter; Escape cancels.
- Terminal: type `DESKTOP` or press Escape/close to return to the desktop.
- System menu: click `XAVIEROS` or press `X`; choose Desktop, Open Terminal, About, Restart, or Shut Down with arrows and Enter.
- Display: choose a mode in Settings and click Apply Mode. Live changes depend on the emulated VGA adapter. Limine has boot-time mode entries if live switching is unsupported.

## Shell commands

Run `HELP` for the current full list. Key groups include `SYSINFO`; `NET`, `NETSTAT`, `NETCHECK`, `PING`; experimental `HTTPGET` and `FETCH`; `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, `NTP`; `CONFIG`, `SETTINGS`, `REPORT`; `LS`, `CAT`, `STAT`, `WRITE`, `MKDIR`, `RM`, `DF`, `FSCK`; `USERADD`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD`; and diagnostic commands such as `HEALTH`, `SELFTEST`, `TASKS`, `HEAP`, and `MEM`.

## Documentation

- [Changelog](CHANGELOG.md)
- [MIT License](LICENSE)

The root documentation is intentionally kept to `README.md`, `README.ru.md`, and `CHANGELOG.md`. A release kit includes a longer cumulative release-notes document.

## Status and limitations

XavierOS is experimental and is not a general-purpose or secure operating system. The desktop and its applications run inside the kernel rather than isolated user processes. Files supports directory navigation, bounded text preview, and selected operations on `/DISK`, but is not a complete file manager/editor. Network features target the QEMU E1000 and user-mode NAT; HTTPS/TLS, IPv6, and general-purpose networking are not implemented. XVDISKFS is not journaled or crash-safe. Live resolution switching depends on VGA/Bochs VBE support. Restart and shutdown are emulator-oriented; physical hardware is not validated.

**Run XavierOS in QEMU or another disposable virtual machine. Do not rely on it to protect important data.**

## License

XavierOS code and documentation are distributed under the [MIT License](LICENSE). Third-party tools and components remain under their respective licenses.

## Existing shell, storage, and network limits

- `SYSINFO` keeps its neofetch-style `X` logo and reports CPU vendor, display, user, uptime, PMM, heap, disk, cooperative scheduler, and shell details. The neofetch-style `SYSINFO` view deliberately keeps uptime and display on separate rows. `HEALTH`, `DF`, and related diagnostics expose basic subsystem health.
- `FSCK` is a read-only metadata check and does not repair disk structures. It detects metadata problems but never writes repair changes. `DF` reports virtual-disk usage; back up `data/xavieros-disk.img` before filesystem experiments.
- `CONFIG SET TIMEZONE` changes a manual fixed UTC offset; `TIMEZONE +0300` is the compact offset format. `CONFIG SET NETWORK` chooses DHCP or a static network profile, and `CONFIG SAVE` persists supported settings.
- The DHCP client negotiates a lease for the QEMU E1000 network; the UDP NTP client can synchronize the clock after network setup. RTC is used before successful synchronization. `DATE`, `TIME`, `DATETIME`, `NTP`, and `TIMEZONE` are also available from the shell.
- `REPORT SAVE` writes a diagnostic report to an allowed virtual-disk path. Shell path permissions are not a Ring 3 security boundary.
- Automatic GitHub release checking is not implemented yet; HTTPS/TLS support is required before the kernel can query GitHub securely. The experimental `HTTPGET` and `FETCH` commands support plain HTTP only.
- The shell has Up/Down command history and Tab completion for commands and paths. Accounts use salted password hashes but do not provide isolated user processes.

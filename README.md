<div align="center">

# XavierOS

**A hobby x86_64 operating system written in Rust.**

A `no_std` kernel booting with Limine, with a framebuffer desktop, a shell, storage, and experimental networking.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-x86__64%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-0.2.5%20Pre-release-orange)](#status-and-limitations)

**Current version: `0.2.5`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a learning-oriented hobby operating system written in Rust without the standard library. It boots through Limine and combines a framebuffer desktop with experimental kernel, storage, and network subsystems. Release 0.2.5 adds a RAM backbuffer with 32×32 dirty-tile presentation, a PCI configuration-space enumerator and device inventory, and a modern VirtIO 1.x PCI block driver with the existing ATA PIO path retained as a fallback. It builds on the cooperative kernel-thread scheduler, IRQ-safe locks, and active-address-space VMM introduced in 0.2.4.

## Features

- **Kernel and boot:** `x86_64-unknown-none`, Limine boot protocol, IDT exception handlers, PMM bitmap allocator, HHDM, IRQ-safe heap/PMM synchronization, PIC/PIT, PS/2 keyboard and polled PS/2 mouse.
- **Kernel threads:** bounded 16-slot TCB table, dynamically allocated 16 KiB PMM-backed stacks, priorities 0–7 with round-robin scheduling among equal effective priorities and aging (+1 every 25 READY ticks, capped at 7) for long-waiting tasks, runtime spawn, cooperative yield, sleep/wake, blocked/zombie/finished states, and stack reclamation. Timer interrupts wake sleepers but do not preempt a running thread.
- **Virtual memory:** four-level active page-table walker for 4 KiB, 2 MiB, and 1 GiB mappings; supervisor 4 KiB map/unmap APIs; local `invlpg`; NX enablement when supported. Separate user address spaces, user mappings, page-table reclamation, and SMP TLB shootdowns are not implemented.
- **Kernel diagnostics:** fixed-capacity allocation-free event log (`KLOG`), scheduler/lock statistics (`SCHEDSTAT`), active page-table status (`VMSTAT`), and kernel subsystem summary (`KERNEL`).
- **Login Manager:** first-run administrator creation with password confirmation, a startup account picker, password-protected sign-in, a passwordless `LIVE-USER` guest session, session lock/unlock, switch user, and log out. Keyboard input is enabled before the login UI starts. The custom mouse pointer remains visible and follows PS/2 mouse movement; selecting fields and activating controls still uses the keyboard.
- **Renderer V2:** CPU-rendered UI is drawn into a static RAM backbuffer (up to 1366×768, 32-bit) and presented through a 32×32 dirty-tile map. The desktop keeps a defined painter order—wallpaper, desktop chrome, windows, overlays, cursor—and copies only damaged tiles to the Limine frontbuffer. Text rendering batches dirty-region marking once per text run, and sub-framebuffer damage is clipped to the view bounds. Backbuffer drawing uses ordinary RAM stores; the final frontbuffer copy uses volatile writes. This is software compositing, not GPU acceleration or hardware page flipping.
- **Files:** opens in the current session home (`/DISK/HOME/<username>` or `/DISK/HOME/live`), then supports folder navigation, Back/Up/Home/Disk/System/Favorite, paging, bounded text preview, directory/empty-file creation, and deletion on writable `/DISK`. Back history and Favorite are session-only. The embedded root is read-only; preview is bounded and not a full text editor.
- **Terminal:** existing kernel shell rendered inside a window, with line editing, command history, path/command Tab completion, account commands, filesystem commands, network diagnostics, and system reports. The Terminal takes keyboard focus while running and updates only changed text rows until its visible history scrolls; `CLEAR` forces a full repaint so previous output is erased.
- **PCI and devices:** configuration-mechanism #1 enumeration, BDF/Vendor/Device/Class/IRQ inventory, safe reads of assigned BARs, and a desktop Device Manager panel (`D` shortcut) plus shell views (`PCI`/`DEVICES`, `DRIVERS`). The panel pages through BDF/Vendor/Device/Class records and shows the active storage/network drivers. The existing E1000 probe now consumes the shared PCI inventory. PCI discovery is bounded to 128 stored devices.
- **Clock:** local time, fixed UTC offset, RTC and experimental NTP synchronization controls.
- **Settings:** three display profiles—1024×768, 1280×720, 1366×768—with a guarded Bochs VBE live-mode request when supported. Limine profiles remain the boot-time fallback.
- **About:** overview, kernel, and license tabs.
- **Storage:** read-only XVRFS initramfs plus writable XVDISKFS. The default QEMU configuration attaches a modern VirtIO Block PCI device; the driver validates the modern PCI capability chain and notification layout, then uses one polled split virtqueue for synchronous 512-byte sector I/O. ATA PIO remains available with `python run.py --ata-disk`.
- **Build tooling:** Python scripts for initramfs creation, building, launching and release packaging; host-side regression tests.

## Build and run

Requirements: Rust via `rustup`, Python 3.10+, QEMU (`qemu-system-x86_64`), and the boot-image prerequisites described in the build script (including `xorriso`/Limine tooling where needed).

```powershell
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

The ISO is written under `build/`. `run.py` attaches `data/xavieros-disk.img` as a modern VirtIO Block PCI device. The same image can be attached through legacy IDE/ATA with `python run.py --ata-disk`; both modes use the same raw sectors. Back up the persistent image before experimenting. Do not use `--reset-disk` unless you intentionally want to recreate it.

## Desktop controls

- Mouse: click Dock icons and app controls when a PS/2 auxiliary device is available.
- Keyboard: `F` Files, `T` Terminal, `N` Network, `C` Clock, `S` Settings, `A` About, `D` Device Manager, `X` system menu. Arrow keys navigate the menu, Files list, About tabs, and Device Manager pages; Enter activates; Escape closes or moves up in Files. In Network, `G` checks the gateway and `D` checks DNS. In Clock, `N` syncs NTP, `M`/`P` adjust timezone by one hour, and `S` saves it.
- Files: click a folder to enter it; use `BACK`/Backspace/`B` for history, `UP`/Escape for parent, `PIN`/`P` to save a session favorite and `FAVORITE`/`V` to open it; `PREV`/`NEXT` page the list. Select a file to preview, `NEW DIR` or `NEW FILE` to create an item under `/DISK`, and `DELETE` to remove a selected `/DISK` file. During name entry, type ASCII and press Enter; Escape cancels.
- Terminal: type `DESKTOP` or press Escape/close to return to the desktop.
- System menu: click `XAVIEROS` or press `X`; choose Desktop, Open Terminal, About, Lock Screen, Switch User, Log Out, Restart, or Shut Down with arrows and Enter. Switch User and Log Out return to Login Manager; Log Out first ends the current account session and shows a signed-out notice. Login Manager accepts `Up`/`Down` plus Enter; password fields are masked. First-run setup uses Tab or Enter to move through username, password, and confirmation. In QEMU, Shut Down sends the ACPI poweroff request and the emulator is allowed to exit.
- Display: choose a mode in Settings and click Apply Mode. Live changes depend on the emulated VGA adapter. Limine has boot-time mode entries if live switching is unsupported.

## Shell commands

Run `HELP` for the current full list. Key groups include `SYSINFO`; `NET`, `NETSTAT`, `NETCHECK`, `PING`; experimental `HTTPGET` and `FETCH`; `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, `NTP`; `CONFIG`, `SETTINGS`, `REPORT`; `LS`, `CAT`, `STAT`, `WRITE`, `MKDIR`, `RM`, `DF`, `FSCK`; `USERADD`, `LOGIN`, `LOGOUT`, `WHOAMI`, `PASSWD`; and diagnostic commands such as `HEALTH`, `SELFTEST`, `TASKS`, `TASKRUN`, `SCHEDSTAT`, `VMSTAT`, `VMTEST`, `KLOG`, `KERNEL`, `HEAP`, `MEM`, `PCI`, `DEVICES`, `DRIVERS`, `GFXINFO`, and `GFXTEST`.

## Documentation

- [Changelog](CHANGELOG.md)
- [MIT License](LICENSE)

The root documentation is intentionally kept to `README.md`, `README.ru.md`, and `CHANGELOG.md`. Release Notes describe only the current release; the cumulative development history stays in `CHANGELOG.md`.

## Status and limitations

XavierOS is experimental and is not a general-purpose or secure operating system. Renderer V2 requires a 32-bit framebuffer no larger than 1366×768; unsupported modes use direct framebuffer rendering. It is a CPU software backbuffer with 32×32 dirty tiles, not a GPU compositor, independent retained layer surfaces, or vblank-synchronized page flip. The PCI enumerator uses legacy configuration mechanism #1 and stores at most 128 devices; it does not yet configure PCI bridges, MSI/MSI-X, PCIe extended capabilities, or hotplug. VirtIO Block supports modern PCI common/notify/device capabilities, one split queue, synchronous polled 512-byte requests, and the VERSION_1 feature; interrupts, multiqueue, packed rings, flush/discard negotiation, and queues with fewer than four entries are unsupported. The driver currently relies on DMA pages below 4 GiB. VirtIO Network, APIC/IOAPIC operation, USB xHCI, and VirtIO-GPU are not implemented; detected device classes are informational only. The scheduler TCB table is capped at 16 slots; each task receives a 16 KiB physically contiguous stack without a guard page or stack-overflow detector. Kernel threads are cooperative, so a CPU-bound thread that never yields can stall other threads. The context switch does not save a separate FPU/SIMD context. IRQ-safe spinlocks protect selected shared structures on the current single-core execution model, but the whole system is not SMP-safe. The PMM manages physical pages below 4 GiB only. The VMM operates on the active four-level Limine address space and supervisor mappings only; it refuses five-level paging and does not create isolated user address spaces. Page-table reclamation after unmap and SMP TLB shootdowns are not implemented. The desktop and its applications run inside the kernel rather than isolated user processes. Account UI, password checks, and shell path restrictions are a prototype and are not a security boundary against arbitrary kernel code. Files supports directory navigation, bounded text preview, and selected operations on `/DISK`, but is not a complete file manager/editor. Network features target the QEMU E1000 and user-mode NAT; HTTPS/TLS, IPv6, and general-purpose networking are not implemented. XVDISKFS is not journaled or crash-safe. Live resolution switching depends on VGA/Bochs VBE support. Restart and shutdown are emulator-oriented; QEMU shutdown depends on ACPI support, and physical hardware is not validated.

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
- The shell has Up/Down command history and Tab completion for commands and paths. Accounts use salted password hashes, but do not provide isolated user processes. `LIVE-USER` is intentionally passwordless and can only access its own writable home path through shell-level path checks.

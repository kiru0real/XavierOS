# Changelog

All notable XavierOS changes are documented here. The release-kit file `XavierOS-0.2.0-Release-Notes.md` contains the expanded history and detailed release notes.

## 0.2.0 — Desktop and File Manager Update

### Added
- Navigable Files application with folder navigation, paging, text preview, create-folder, create-empty-file, and delete-selected-file actions under writable `/DISK`.
- Clickable Files sidebar (Home, Disk, System), path bar, action buttons, and keyboard controls.
- Network diagnostics actions for gateway and DNS checks, transmission/reception frame counters, Clock controls for NTP synchronization and timezone offset/save, and About tabs for overview/kernel/license.
- Expanded release-wide documentation including every beta milestone.

### Fixed and improved
- Window underlay is saved before drawing a window and restored on close, replacing the previous full-screen wallpaper repaint that could look like a bottom-to-top printer scan.
- Changes inside an open app redraw its window area rather than the whole desktop, except when the framebuffer dimensions actually change.
- Wallpaper source is 1470×904 generated from the original user-provided image with Lanczos resizing and a restrained unsharp mask; the kernel uses bilinear cover interpolation. This smooths scaling but cannot recreate detail absent from the original image.
- App close via Escape, title-bar close controls, and Terminal return path restore the window underlay.

### Limitations
- Files preview is bounded, does not edit file contents, and only supports create/delete operations in the writable disk mount.
- GUI apps run in kernel space; there is no isolated user-mode GUI process model.
- Live display mode changes depend on QEMU/Bochs VBE support. HTTPS/TLS and IPv6 are not implemented. Physical hardware is not validated.

## 0.2.0 Beta 4 — Smoother Desktop and Windowed Terminal
- Replaced nearest-neighbor wallpaper scaling with fixed-point bilinear interpolation.
- Unified Files, Network, Clock, and About panels with a common visual language.
- Rendered the existing interactive shell inside the Terminal window while retaining the current command editor, history, and completion.
- Added incremental window/cursor redraws and window close controls.

## 0.2.0 Beta 3 — Compact Cursor and Display Settings
- Reduced the custom cursor rendering size to 28×28.
- Added selectable resolution cards and apply/current controls in Settings.
- Added guarded Bochs VBE live mode switching for supported QEMU VGA configurations and handling for unsupported modes.

## 0.2.0 Beta 2 — Custom Cursor
- Integrated the transparent custom cursor image as an RGBA framebuffer resource.
- Added contrast treatment and background save/restore for cursor drawing.
- Updated build invalidation to include changes under `assets/`.

## 0.2.0 Beta 1 — First Desktop
- Added the first framebuffer desktop, wallpaper, top bar, Dock, six application launch targets, and polled PS/2 mouse support.
- Fixed initial cursor-motion flicker by avoiding a full desktop redraw for every pointer update.
- Drained PS/2 mouse packets while the shell waits for keyboard input, reducing input-controller contention.
- Added Limine display profiles for 1024×768, 1280×720, and 1366×768.

## Earlier kernel milestones
- 0.1.8: persistent config slots with checksums, safer FETCH temp-file/commit flow, path/access validation, network timeouts, and diagnostic REPORT command.
- 0.1.7: experimental TCP/HTTP improvements, FETCH safeguards, retry/counter work, and safer response handling.
- 0.1.6: experimental TCP/HTTP client, HTTPGET, FETCH foundations, NETSTAT/NETCHECK improvements, and NTP refinements.
- 0.1.5: DHCP, RTC/NTP wall clock, DATE/TIME/DATETIME/TIMEZONE commands, and compact timezone offset input.
- 0.1.4: E1000 networking, ARP/IPv4/ICMP/PING/DNS foundations, MMIO page-fault correction, and SYSINFO output formatting fix.
- 0.1.3: shell and security improvements, FSCK/DF/HEALTH diagnostics, and preserved neofetch-style pixel X logo.
- 0.1.2: persistent user accounts, password login, home directory, command history, and Tab completion.
- 0.1.0: initial public kernel release.

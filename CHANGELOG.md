# Changelog

All notable XavierOS changes are documented here. Each release-kit Release Notes file describes only that release. This changelog retains the cumulative development history.

## 0.2.3 — Rendering and Responsiveness

### Fixed
- App updates now redraw the opaque application window in place instead of restoring the wallpaper before every refresh, reducing visible flashes.
- System-menu open, close, and selection changes now save/restore only the menu overlay area. Opening Terminal from the system menu restores the underlying desktop before saving the Terminal window background.
- Clock changes refresh only the clock text and top-bar time once per displayed minute; the Network panel is no longer repainted by the periodic clock tick.
- Terminal command output is appended by drawing only changed text rows. A full Terminal redraw is retained when the visible viewport actually scrolls or the bounded line history rolls over.

### Improved
- Wallpaper scaling keeps fixed-point bilinear interpolation while reusing precomputed framebuffer channel lookup tables and incrementally advancing source coordinates.
- Opaque custom cursor pixels skip destination reads and alpha-blending calculations.
- Dock selection is updated locally when an app is selected, without repainting the wallpaper.

### Verification
- Host-side Python tests cover packaging helpers and source-level rendering invariants. They do not compile the Rust kernel or prove QEMU runtime behavior; Rust compilation and visual flicker checks still require a local QEMU run.

## 0.2.2 — Session and Power Actions Fix

- Log Out now ends the current account session and returns to Login Manager with a visible signed-out notice; Switch User uses the same account picker with a distinct notice.
- The QEMU launcher no longer passes `-no-shutdown`, allowing the emulator to exit when XavierOS sends its ACPI poweroff request.
- Fixed first-run account setup keyboard input by enabling IRQ0/IRQ1 before entering the graphical Login Manager.
- Added the custom mouse pointer to first-run setup, account selection, password sign-in, and session lock screens, plus a visible insertion caret in first-run text fields.
- 70 host-side Python tests pass after extracting the updated source archive. They cover source-level invariants and build/package helpers; they do not compile Rust or prove GUI runtime behavior. Rust compilation and QEMU interaction still require local verification before publication.

## 0.2.2 — Accounts & Sessions Update

### Added
- Added a framebuffer Login Manager at startup, with a first-run wizard for the initial administrator account and password confirmation.
- Added an account picker with password-masked sign-in and an explicit passwordless `LIVE-USER` guest option.
- Added system-menu actions for Lock Screen, Switch User, and Log Out. Lock Screen verifies the current account password before resuming the same session.
- Added a password verification path for unlocking a locked session and retained the existing account database format.
- Restricted the passwordless guest session to `/DISK/HOME/live` for writable-disk paths enforced by the shell-level path checks.

### Limitations
- Login Manager controls are keyboard-driven; mouse-click account selection is not implemented yet.
- Account checks and file-path checks execute in ring 0 and are not real user-mode isolation. Applications and shell commands remain kernel code.
- Rust compilation and QEMU runtime behavior still require local verification; the packaging environment has no Rust toolchain or QEMU.

## 0.2.1 — Files interaction fix and Quick Access

### Fixed
- Added a dedicated **BACK** action backed by an eight-step in-session folder history; `Backspace` and `B` provide keyboard equivalents. `UP` and `Escape` continue to navigate to the parent directory.
- Added small hit-target tolerance to compact toolbar controls to make PS/2 mouse interaction less finicky.
- Made Files status feedback visible independently of the currently selected file preview, so errors such as “writes require /DISK” are no longer hidden behind a selected item.
- Added clear feedback for Previous/Next when the current directory has no additional pages.

### Added
- Added a session-pinned Favorite folder: use **PIN** (or `P`) in the current directory and **FAVORITE** in the sidebar (or `V`) to jump back to it.
- Added tests covering visible button labels, Back/history routing, Favorite pin/open actions, and visible status feedback.

### Limitations
- The folder history and favorite are in-memory for the current Files session and are not persistent across reboot.
- Writable create/delete actions remain limited to `/DISK`; embedded root files are read-only.
- Rust compilation and QEMU runtime behavior must be confirmed locally because those tools are unavailable in the packaging environment.

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

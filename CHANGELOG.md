## 0.2.0-beta.4 — Smoother Desktop and Windowed Terminal

### ✨ What's New
- Replaced nearest-neighbour wallpaper scaling with fixed-point bilinear interpolation to reduce visible pixelation at common QEMU resolutions.
- Unified Files, Network, Clock and About panels with the same sidebar, heading, status-card and information-card design language as Settings.
- Changed Terminal from a full-screen framebuffer console to an in-window shell surface that preserves the existing command set, line editor, command history and Tab completion.
- Kept mouse movement active while Terminal is open and added close controls through Escape and the Terminal title-bar close button. Terminal currently owns keyboard focus while the shell is active.
- Updated kernel, package, shell, rootfs and documentation version metadata to `0.2.0-beta.4`.

### 🐛 Fixes
- Windowed Terminal now draws only into a sub-framebuffer inside its application window instead of taking over the entire screen.
- Terminal console text layout adapts to the window content size and uses a compact font scale to improve the visible command history.
- The wallpaper's source pixels are interpolated rather than repeatedly magnified as hard-edged blocks.

### 📦 Release
`0.2.0-beta.4` is a development beta. Host-side tests and archive-integrity checks do not replace a Rust build and QEMU runtime test. Verify drawing, command input, mouse movement and closing Terminal in QEMU before publishing.

---

# XavierOS Changelog

## 0.2.0-beta.3 — Compact Cursor and Display Settings

- Reduced the in-kernel pointer render target from 64×64 to 28×28 pixels while keeping the original transparent 64×64 PNG as the source asset.
- Rebuilt the embedded RGBA sprite with high-quality downsampling and scaled the pointer hotspot and framebuffer save/restore region to match.
- Replaced the text-only Settings preview with a redesigned Display & Resolution panel, three selectable mode cards, Apply Mode and Use Current controls, active-mode information, and clear success/error feedback.
- Added guarded Bochs VBE mode switching for QEMU's standard VGA adapter. The kernel verifies the adapter ID and available video memory, checks mode readback, and tries to restore the old mode if the request is rejected.
- Made `run.py` request `-vga std` explicitly so the supported QEMU VBE adapter is predictable. Other video adapters may report that live switching is unavailable; Limine boot-time profiles remain available.
- Added keyboard selection (left/right) and Enter-to-apply while Settings is open.
- Updated package/kernel/shell/rootfs metadata to `0.2.0-beta.3`.
- Host-side checks validate the 28×28 sprite data, the clickable resolution controls and the QEMU VBE code path. Rust compilation and QEMU runtime still require confirmation on the developer's machine.


## 0.2.0-beta.2 — Custom Cursor Polish

- Added the supplied 64×64 transparent PNG pointer and embedded alpha-preserving RGBA data for the no_std framebuffer renderer.
- Added a light contrast halo so the black pointer remains visible on dark wallpaper and bright windows.
- Expanded cursor save/restore to cover the sprite, including clipping at the framebuffer edges, while preserving incremental redraw to avoid desktop flicker.
- Updated all current build, shell, diagnostics, package and documentation version metadata to `0.2.0-beta.2`.
- Updated build invalidation so changes to `assets/` trigger a rebuild instead of accidentally reusing an older ISO.
- Preserved the original PNG in `assets/cursor.png`; its transparent background was already present and did not need removal.
- This release is prepared for local testing only; Rust build and QEMU runtime still need confirmation on the development host.

## 0.2.0-beta.1 — Desktop Preview

- Added the first framebuffer desktop with a menu bar, full-screen cover-scaled scenic wallpaper, redesigned glass-style dock and six custom vector app icons.
- Added a polled PS/2 mouse path with keyboard navigation as a fallback.
- Fixed desktop flicker during mouse movement by restoring/redrawing only the cursor region instead of repainting the full desktop for every mouse packet.
- Fixed Terminal keyboard starvation by draining the polled PS/2 mouse stream while the shell and password prompts wait for keyboard input.
- Added Files, Network, Clock, Settings and About preview windows; Terminal opens the existing shell. Settings displays the active framebuffer resolution.
- Added Limine startup entries for 1024x768, 1280x720 and 1366x768. Live in-kernel resolution switching remains unsupported until a runtime video-mode driver exists.
- Added `DESKTOP` to return from Terminal to the desktop.
- Updated version metadata and documentation for `0.2.0-beta.1`.
- This is an early in-kernel UI prototype; desktop apps are not isolated user processes.

## 0.1.8 — Persistence and Reliability

- Added a checksummed persistent configuration file with alternating slots to preserve the previous valid copy when a save fails.
- Added `CONFIG`/`SETTINGS` commands for timezone, preferred NTP server, DHCP/static mode, network timeout, saving, and resetting settings.
- Added `REPORT` to print a diagnostic report or save it to an accessible disk path.
- Changed `FETCH` to write to a temporary file and rename it only after receiving a complete valid response; existing destination files are not overwritten.
- Tightened disk-path traversal rejection before administrator/live-session permission shortcuts.
- Ensured the console respects the saved DHCP/static mode instead of unconditionally requesting DHCP a second time when the shell starts.
- Added regression tests for configuration, report generation, transactional fetch, and path traversal.
- Automatic update notifications are deferred until HTTPS/TLS with certificate validation is implemented; the OS does not contact GitHub over insecure HTTP.

## 0.1.7 — HTTP Downloads and Network Hardening

- Added bounded TCP SYN and HTTP request retries with retry counters.
- Added HTTP redirect following (up to three redirects; HTTP only).
- Added `Content-Length` handling, chunked response decoding, and rejection of ambiguous framing headers.
- Added `FETCH` to save a completed small HTTP response body under `/DISK`, subject to user path permissions.
- Expanded `NETSTAT` with TCP retries and HTTP request/success/failure/byte counters.
- Added regression coverage for chunk parsing, redirect target construction, duplicate headers, and FETCH permission/size checks.
- Fixed a Rust type mismatch when appending relative redirect paths to the HTTP URL buffer.

## 0.1.6 — Networking and Diagnostics

- Added a bounded, one-shot TCP client for plain HTTP/1.0 requests through the QEMU E1000 network stack.
- Added `HTTPGET`, `NETSTAT`, and `NETCHECK` console commands.
- Added TCP pseudo-header checksum validation, sequence/acknowledgement checks, bounded receive buffers, response-size limits, and connection timeouts.
- Added transmit/receive frame counters, transmit-error counts, and dropped-frame counters.
- Tightened IPv4 fragment handling and NTP response timestamp validation.
- Added a single retry for transient NTP synchronization failures while preserving the last known good clock if a retry fails.
- Updated documentation for HTTP/TCP limitations and the new network commands.

## 0.1.5 — DHCP and Wall Clock

- Added DHCP configuration, RTC/NTP wall-clock commands, and manual fixed UTC offsets.
- Added static QEMU network fallback when DHCP fails.
- Added `DATE`, `TIME`, `DATETIME`, `TIMEZONE`, and `NTP`.

## 0.1.4 — Experimental Networking

- Added QEMU E1000 networking, ARP, IPv4, ICMP echo, DNS A queries, and NTP over UDP.
- Improved PING statistics and reported actual TTL values.

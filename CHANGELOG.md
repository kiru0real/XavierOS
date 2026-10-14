# Changelog

## 0.1.3 — Shell, account and filesystem maintenance

- Added input cursor editing: Left/Right, Home/End, Delete, Ctrl+U and Ctrl+C.
- Changed the shell input cursor from an underline to a thin vertical caret.
- Fixed `SYSINFO` row termination: `UPTIME` and `DISPLAY` now render on separate lines, preventing the next X fragment and display label from being concatenated into one row.
- Kept the pixel-style X at exactly 15 rows, with no repeated pattern below it; remaining system fields stay aligned in the same column.
- Added temporary login rate limiting after repeated failed sign-in attempts.
- Removed the predictable timer/TSC password-salt fallback; account creation and password changes now require RDRAND.
- Added read-only XVDISKFS consistency check (`FSCK`) and space summary (`DF`).
- Expanded neofetch-style `SYSINFO` and added `HEALTH` and `UPTIME` commands.
- Updated host tests and compact bilingual documentation.

## 0.1.2 — Local user accounts and shell usability

- Added persistent local accounts, password login/logout, and per-user home directories.
- Added command history and Tab completion for accessible command and path names.

## 0.1.1 — Maintenance

- Improved build tooling, validation and host-side tests.

## 0.1.0 — First public release

- Initial Rust x86_64 kernel, framebuffer console, memory management, interrupts and experimental filesystems.

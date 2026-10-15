# Changelog

## 0.1.5
- `TIMEZONE` accepts compact offsets such as `+0300` and `-0530` when entering a colon is inconvenient.

## 0.1.5 — DHCP and wall clock (development)

- Added DHCP DISCOVER/OFFER/REQUEST/ACK with validated leases and static fallback.
- Added `DATE`, `TIME`, and `DATETIME`, using CMOS RTC until NTP sync.
- Added manual fixed-offset `TIMEZONE` command and `NTP [SERVER]` synchronization.
- Added bounds-checked DHCP/NTP parsing and host-side regression checks.
- `SYSINFO` is unchanged; wall-clock information is shown only by explicit clock commands.

## 0.1.4 — Network polish

- Improved `PING` timing: uses the TSC frequency from CPUID leaf 0x15 when available to report sub-millisecond RTT; falls back to an honest 10 ms PIT-resolution indication instead of reporting a misleading `0 ms`.
- `PING` now sends four probes by default; optional count supports 1–8 probes, unique ICMP sequence numbers, loss statistics and per-probe errors.
- Parses ICMP destination-unreachable and time-exceeded errors when they quote the matching probe.
- Reports TTL from the received IPv4 header rather than a fixed display value. QEMU user-mode networking may synthesize/proxy ICMP responses, so TTL is not necessarily the remote physical host's TTL.

## 0.1.4 — First experimental networking

- Added QEMU E1000 initialization with DMA RX/TX rings and polled Ethernet frame I/O.
- Added ARP, static QEMU-usernet IPv4 profile, ICMP echo (`PING`), and DNS A-record lookup.
- Added `NET` for NIC/link/address diagnostics and attached `e1000` to the portable QEMU run script.
- Fixed an early PAGE FAULT by explicitly mapping the PCI E1000 MMIO register window in the active HHDM page tables before volatile register access.
- Fixed the neofetch-style `SYSINFO` row break so `DISPLAY` no longer merges into `UPTIME`.
- Updated all active version strings and release packaging metadata to 0.1.4.

## 0.1.3 — Shell, account and filesystem maintenance

- Added input cursor editing: Left/Right, Home/End, Delete, Ctrl+U and Ctrl+C.
- Changed the shell input cursor from an underline to a thin vertical caret.
- Fixed `SYSINFO` logo layout: the pixel-style X now uses exactly 15 rows, with no blank gap or repeated pattern below it; remaining system fields stay aligned in the same column.
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

# Changelog

## 0.1.4 — Experimental networking

- Added QEMU E1000 initialization with DMA RX/TX rings and polled Ethernet frame I/O.
- Added ARP, static QEMU user-network IPv4 configuration, ICMP echo (`PING`), and DNS A-record lookup.
- Added `NET` for NIC/link/address diagnostics and attached an emulated E1000 adapter in the portable QEMU run script.
- Explicitly maps the E1000 PCI MMIO register window before volatile register access, fixing an early page fault.
- Improved `PING` with multiple probes (1–8), unique sequence numbers, packet-loss statistics, ICMP unreachable/time-exceeded diagnostics, received-packet TTL, and TSC-based RTT timing when supported.
- Fixed the `SYSINFO` row break so `DISPLAY` no longer merges into `UPTIME` or overlaps the X logo.

## 0.1.3 — Shell, account and filesystem maintenance

- Added input cursor editing: Left/Right, Home/End, Delete, Ctrl+U and Ctrl+C.
- Changed the shell input cursor from an underline to a thin vertical caret.
- Added login rate limiting, read-only filesystem consistency checks (`FSCK`), disk usage (`DF`), `HEALTH` and `UPTIME`.
- Account creation and password changes require RDRAND instead of falling back to predictable timer/TSC-derived salts.

## 0.1.2 — Local user accounts and shell usability

- Added persistent local accounts, password login/logout, and per-user home directories.
- Added command history and Tab completion for accessible command and path names.

## 0.1.1 — Maintenance

- Improved build tooling, validation and host-side tests.

## 0.1.0 — First public release

- Initial Rust x86_64 kernel, framebuffer console, memory management, interrupts and experimental filesystems.

# Changelog

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

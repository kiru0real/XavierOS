# Changelog

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

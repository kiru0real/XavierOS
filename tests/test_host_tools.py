from __future__ import annotations

import io
import stat
import struct
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import build
import mkinitramfs
import package_release


class ArchiveExtractionTests(unittest.TestCase):
    def _make_tar(self, path: Path, members: list[tuple[str, bytes, str | None]]) -> None:
        with tarfile.open(path, "w:gz") as archive:
            for name, content, linkname in members:
                info = tarfile.TarInfo(name)
                if linkname is not None:
                    info.type = tarfile.SYMTYPE
                    info.linkname = linkname
                    archive.addfile(info)
                else:
                    info.size = len(content)
                    archive.addfile(info, io.BytesIO(content))

    def test_tar_extracts_regular_file(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "sample.tar.gz"
            output = root / "out"
            self._make_tar(archive, [("bin/tool", b"hello", None)])
            build.safe_extract_tar(archive, output)
            self.assertEqual((output / "bin" / "tool").read_bytes(), b"hello")

    def test_tar_rejects_parent_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "bad.tar.gz"
            self._make_tar(archive, [("../outside.txt", b"no", None)])
            with self.assertRaises(RuntimeError):
                build.safe_extract_tar(archive, root / "out")
            self.assertFalse((root / "outside.txt").exists())

    def test_tar_rejects_case_insensitive_duplicates(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "duplicate.tar.gz"
            self._make_tar(archive, [("Tool", b"one", None), ("tool", b"two", None)])
            with self.assertRaises(RuntimeError):
                build.safe_extract_tar(archive, root / "out")

    def test_tar_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "link.tar.gz"
            self._make_tar(archive, [("link", b"", "../../outside")])
            with self.assertRaises(RuntimeError):
                build.safe_extract_tar(archive, root / "out")

    def test_zip_extracts_regular_file(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "sample.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("bin/tool.txt", b"hello")
            build.safe_extract_zip(archive, root / "out")
            self.assertEqual((root / "out" / "bin" / "tool.txt").read_bytes(), b"hello")

    def test_zip_rejects_parent_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "bad.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("../outside.txt", b"no")
            with self.assertRaises(RuntimeError):
                build.safe_extract_zip(archive, root / "out")
            self.assertFalse((root / "outside.txt").exists())

    def test_zip_rejects_case_insensitive_duplicates(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "duplicate.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("Tool", b"one")
                output.writestr("tool", b"two")
            with self.assertRaises(RuntimeError):
                build.safe_extract_zip(archive, root / "out")

    def test_zip_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "link.zip"
            info = zipfile.ZipInfo("link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr(info, "../../outside")
            with self.assertRaises(RuntimeError):
                build.safe_extract_zip(archive, root / "out")


class InitramfsBuilderTests(unittest.TestCase):
    def test_image_layout_and_file_payload(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            rootfs = root / "rootfs"
            (rootfs / "ETC").mkdir(parents=True)
            (rootfs / "README.TXT").write_bytes(b"hello XavierOS")
            (rootfs / "ETC" / "ISSUE").write_bytes(b"XavierOS test\n")
            output = root / "initramfs.xfs"
            mkinitramfs.build(rootfs, output)
            image = output.read_bytes()
            magic, count, table_offset = struct.unpack_from("<8sII", image, 0)
            self.assertEqual(magic, b"XVRFS001")
            self.assertEqual(count, 3)
            paths: dict[str, bytes] = {}
            for index in range(count):
                offset = table_offset + index * struct.calcsize("<B3xIHHII")
                kind, path_offset, path_len, _reserved, data_offset, data_len = struct.unpack_from("<B3xIHHII", image, offset)
                path = image[path_offset:path_offset + path_len].decode("utf-8")
                payload = image[data_offset:data_offset + data_len] if kind == mkinitramfs.KIND_FILE else b""
                paths[path] = payload
            self.assertIn("ETC", paths)
            self.assertEqual(paths["README.TXT"], b"hello XavierOS")
            self.assertEqual(paths["ETC/ISSUE"], b"XavierOS test\n")

    def test_rejects_case_insensitive_collisions(self) -> None:
        # This tests the collision policy directly because the host filesystem may
        # (as on default Windows volumes) be unable to store Foo.txt and foo.TXT
        # as two distinct entries in the same directory.
        with self.assertRaises(ValueError):
            mkinitramfs._validate_case_insensitive_paths(["Foo.txt", "foo.TXT"])

    def test_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = Path(temp).parent / "xavieros-test-outside.txt"
            outside.write_text("no", encoding="utf-8")
            try:
                (root / "link.txt").symlink_to(outside)
            except (OSError, NotImplementedError):
                self.skipTest("Symlinks are not supported by this host account")
            with self.assertRaises(ValueError):
                mkinitramfs.build(root, Path(temp) / "out.xfs")
            outside.unlink(missing_ok=True)


class ReleasePackagingTests(unittest.TestCase):
    def test_packages_iso_and_sha256(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source.iso"
            source.write_bytes(b"test ISO bytes")
            iso, sums = package_release.package_release(source, root / "dist")
            self.assertEqual(iso.name, "xavieros-0.1.8.iso")
            self.assertEqual(iso.read_bytes(), source.read_bytes())
            digest = package_release.sha256_file(iso)
            self.assertEqual(sums.read_text(encoding="ascii"), f"{digest}  {iso.name}\n")


class RepositoryShapeTests(unittest.TestCase):
    def test_timezone_accepts_compact_offset_without_colon(self) -> None:
        clock = (ROOT / "src" / "clock.rs").read_text(encoding="utf-8")
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("bytes.len() == 5", clock)
        self.assertIn("+HHMM|-HHMM", main)
        self.assertIn("TIMEZONE +0300", readme)

    def test_version_metadata_is_consistent(self) -> None:
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        package = (ROOT / "package_release.py").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn('version = "0.1.8"', cargo)
        self.assertIn('VERSION = "0.1.8"', package)
        self.assertIn("Current version: `0.1.8`", readme)

    def test_only_core_markdown_docs_remain_in_source_tree(self) -> None:
        docs = {path.relative_to(ROOT).as_posix() for path in ROOT.rglob("*.md")}
        self.assertEqual(docs, {"README.md", "README.ru.md", "CHANGELOG.md"})

    def test_shell_history_and_tab_completion_are_wired_into_kernel(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        self.assertIn("COMMAND_HISTORY_CAPACITY: usize = 16", main)
        self.assertIn("0x48 => Some(KeyboardKey::HistoryUp)", main)
        self.assertIn("0x50 => Some(KeyboardKey::HistoryDown)", main)
        self.assertIn("0x0F => Some(KeyboardKey::Tab)", main)
        self.assertIn("fn complete_input(", main)
        self.assertIn("users::may_access_path(candidate_str)", main)
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("Up/Down", readme)
        self.assertIn("Tab completion", readme)

    def test_account_subsystems_are_wired_into_kernel(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        self.assertIn("mod crypto;", main)
        self.assertIn("mod users;", main)
        self.assertIn('"USERADD" => command_user_add', main)
        self.assertIn('"LOGIN" => command_login', main)
        self.assertTrue((ROOT / "src" / "users.rs").is_file())
        self.assertTrue((ROOT / "src" / "crypto.rs").is_file())

    def test_shell_line_editor_controls_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        for feature in ("CursorLeft", "CursorRight", "CursorHome", "CursorEnd", "Delete", "ClearLine", "Cancel"):
            self.assertIn(f"KeyboardKey::{feature}", main)
        self.assertIn("fn redraw_input(", main)
        self.assertIn("let cursor_height = (7 * self.scale).min(self.line_height);", main)
        self.assertNotIn("self.line_height.saturating_sub(3);", main)

    def test_disk_fsck_df_and_health_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        diskfs = (ROOT / "src" / "diskfs.rs").read_text(encoding="utf-8")
        self.assertIn('"FSCK" =>', main)
        self.assertIn('"DF" =>', main)
        self.assertIn('"HEALTH" =>', main)
        self.assertIn("pub fn check_disk()", diskfs)
        self.assertIn("OVERLAPPING FILE EXTENTS ON DISK", diskfs)

    def test_sysinfo_keeps_neofetch_style_and_health_dashboard(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn('sysinfo_row(console, "XX              XX", "XAVIEROS 0.1.8")', main)
        show_start = main.index("fn show_sysinfo(")
        show_end = main.index("fn ", show_start + 3)
        show = main[show_start:show_end]
        self.assertIn('sysinfo_prefix(console, "     XX    XX     "); console.write("UPTIME: "); write_uptime(console); console.write_line("");', show)
        self.assertIn('sysinfo_prefix(console, "    XX      XX    "); console.write("DISPLAY: ");', show)
        self.assertIn('sysinfo_prefix(console, "                  "); console.write("HEAP: ");', show)
        self.assertIn('sysinfo_prefix(console, "                  "); console.write("DISK: ");', show)
        self.assertIn('sysinfo_row(console, "                  ", "SCHEDULER: COOPERATIVE")', show)
        self.assertIn('sysinfo_row(console, "                  ", "SHELL: XAVIER CONSOLE")', show)
        for fact in ("CPU VENDOR:", "DISPLAY:", "USER:", "UPTIME:", "PMM:", "HEAP:", "DISK:"):
            self.assertIn(fact, main)
        self.assertIn("neofetch-style `SYSINFO`", readme)
        self.assertIn('"HEALTH" => show_health', main)

    def test_network_driver_and_qemu_nic_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        run = (ROOT / "run.py").read_text(encoding="utf-8")
        self.assertIn("mod net;", main)
        self.assertIn('"NET" => show_network(console)', main)
        self.assertIn('"PING" => {', main)
        for feature in ("probe_e1000", "REG_RDBAL", "REG_TDBAL", "resolve_mac", "resolve_hostname", "pub fn ping"):
            self.assertIn(feature, net)
        # PCI BAR addresses are not necessarily covered by Limine's RAM HHDM mappings.
        # Explicitly map the E1000 register window before the first volatile MMIO read/write.
        self.assertIn("pub fn map_mmio_region(physical_start: u64, length: u64)", (ROOT / "src" / "memory.rs").read_text(encoding="utf-8"))
        map_call = net.index("memory::map_mmio_region(bar_address, 0x6000)")
        pointer_translation = net.index("memory::physical_to_virtual(bar_address)")
        first_mmio_write = net.index("mmio_write(mmio_ptr, REG_IMC, u32::MAX)")
        self.assertLess(map_call, pointer_translation)
        self.assertLess(pointer_translation, first_mmio_write)
        self.assertIn('"-netdev", "user,id=xvnet0", "-device", "e1000,netdev=xvnet0"', run)

    def test_sysinfo_uptime_and_display_are_separate_rows(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        start = main.index("fn show_sysinfo(")
        end = main.index("fn ", start + 3)
        show = main[start:end]
        uptime = show.index('write_uptime(console); console.write_line("");')
        display = show.index('console.write("DISPLAY: ")')
        self.assertLess(uptime, display)
        self.assertIn('VERSION: 0.1.8', main)

    def test_fsck_scans_without_writing(self) -> None:
        diskfs = (ROOT / "src" / "diskfs.rs").read_text(encoding="utf-8")
        start = diskfs.index("pub fn check_disk()")
        end = diskfs.index("pub fn format()", start)
        check_body = diskfs[start:end]
        self.assertIn("ata::read_sector", check_body)
        self.assertNotIn("ata::write_sector", check_body)
        self.assertIn("FSCK` is a read-only metadata check", (ROOT / "README.md").read_text(encoding="utf-8"))

    def test_fsck_and_df_document_their_safety_boundary(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("does not repair", readme)
        self.assertIn("FSCK` is a read-only metadata check", readme)
        self.assertIn("DF", readme)

    def test_account_auth_is_throttled_and_weak_salt_fallback_removed(self) -> None:
        users = (ROOT / "src" / "users.rs").read_text(encoding="utf-8")
        run_script = (ROOT / "run.py").read_text(encoding="utf-8")
        self.assertIn("FAILED_LOGIN_LIMIT: u8 = 3", users)
        self.assertIn("LOGIN_LOCKOUT_TICKS", users)
        self.assertIn("pub fn secure_salt_available()", users)
        self.assertNotIn("rdtsc", users.lower())
        self.assertIn('"-cpu", "max"', run_script)


    def test_ping_polish_uses_real_headers_precise_timing_and_bounds(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("PING <IPV4 ADDRESS OR HOSTNAME> [COUNT 1-8]", main)
        self.assertIn("PING STATS: SENT=", main)
        self.assertIn("net::resolve_target(target)", main)
        self.assertIn("net::ping_address(address, sequence as u16)", main)
        self.assertIn("pub rtt_microseconds: Option<u64>", net)
        self.assertIn("fn tsc_frequency_hz() -> Option<u64>", net)
        self.assertIn("ttl: ip.ttl", net)
        self.assertIn("quoted_echo_matches(icmp_packet, address, sequence)", net)
        self.assertIn("destination host is unreachable", net)
        self.assertIn("request timed out after 3 seconds", net)

    def test_ping_timing_never_claims_zero_ms_from_coarse_pit(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("<10 MS (PIT RESOLUTION)", main)
        self.assertIn("elapsed_ticks: u64", net)
        self.assertNotIn("elapsed_ms: elapsed", net)

    def test_dhcp_client_and_static_fallback_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn('"DHCP" =>', main)
        self.assertIn("pub fn acquire_dhcp()", net)
        self.assertIn("make_dhcp_message(1, xid, mac, None, None)", net)
        self.assertIn("make_dhcp_message(3, xid, mac, Some(offer.your_ip), Some(server))", net)
        self.assertIn("CONFIG_DHCP.store(1, Ordering::Release)", net)
        self.assertIn("Saved network mode is DHCP; requesting a lease", main)
        self.assertIn("let network_settings = config::current();", main)

    def test_ntp_and_wall_clock_commands_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        clock = (ROOT / "src" / "clock.rs").read_text(encoding="utf-8")
        self.assertIn("mod clock;", main)
        self.assertIn('"NTP" =>', main)
        self.assertIn('"DATE" => show_clock_date(console)', main)
        self.assertIn('"TIME" => show_clock_time(console)', main)
        self.assertIn('"DATETIME" => show_datetime(console)', main)
        self.assertIn('"TIMEZONE" =>', main)
        self.assertIn("pub fn ntp_query(server_name: &str)", net)
        self.assertIn("pub fn synchronize(server: &str)", clock)
        self.assertIn("fn read_stable_rtc()", clock)
        self.assertIn("manual fixed UTC offset", (ROOT / "README.md").read_text(encoding="utf-8"))

    def test_qemu_rtc_uses_utc_and_network_version_is_consistent(self) -> None:
        run = (ROOT / "run.py").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn('"-rtc", "base=utc"', run)
        self.assertIn('version = "0.1.8"', (ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertIn('VERSION = "0.1.8"', (ROOT / "package_release.py").read_text(encoding="utf-8"))
        self.assertIn("DHCP client", readme)
        self.assertIn("UDP NTP client", readme)


    def test_tcp_http_client_is_bounded_and_documents_tls_limit(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        self.assertIn("pub fn http_get(url: &str, output: &mut [u8])", net)
        self.assertIn("fn parse_http_url(url: &str)", net)
        self.assertIn("fn parse_tcp_view", net)
        self.assertIn("tcp_checksum_ipv4", net)
        self.assertIn("network_timeout_ticks()", net)
        self.assertIn("pub fn set_timeout_seconds", net)
        self.assertIn('"HTTPGET" =>', main)
        self.assertIn("HTTPS/TLS IS NOT IMPLEMENTED", main)

    def test_network_diagnostics_and_packet_counters_are_wired(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        for command in ('"NETSTAT" =>', '"NETCHECK" =>'):
            self.assertIn(command, main)
        for field in ("TX_FRAME_COUNT", "RX_FRAME_COUNT", "TX_ERROR_COUNT", "RX_DROP_COUNT"):
            self.assertIn(field, net)
        self.assertIn("pub fn check_gateway()", net)
        self.assertIn("pub fn check_dns()", net)
        self.assertIn("NETCHECK: ALL CHECKS PASSED", main)

    def test_http_url_parser_rejects_https_bad_ports_and_userinfo(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn('"only plain HTTP is supported; HTTPS/TLS is not implemented"', net)
        self.assertIn("authority.contains('@')", net)
        self.assertIn('port == 0 || port > u16::MAX as u32', net)
        self.assertIn('path.len() > 384', net)

    def test_ipv4_fragmented_packets_are_rejected(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("fragment_field & 0x3FFF != 0", net)
        self.assertIn("We do not reassemble fragmented IPv4 datagrams", net)

    def test_ntp_retries_transient_failures_and_validates_timestamps(self) -> None:
        clock = (ROOT / "src" / "clock.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("Retry once for transient packet loss/timeouts", clock)
        self.assertIn("transmit_seconds < receive_seconds", net)
        self.assertIn("NTP server returned an invalid timestamp", net)

    def test_version_017_includes_fetch_and_release_metadata(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        release = (ROOT / "package_release.py").read_text(encoding="utf-8")
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn("HTTPGET", readme)
        self.assertIn("NETSTAT", readme)
        self.assertIn("Current version: `0.1.8`", readme)
        self.assertIn('VERSION = "0.1.8"', release)
        self.assertIn('version = "0.1.8"', cargo)


    def test_relative_redirect_path_uses_byte_slice_for_request_buffer(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("append_bytes(output, &mut used, path[..directory_end].as_bytes())?;", net)
        self.assertNotIn("append_bytes(output, &mut used, &path[..directory_end])?;", net)

    def test_http_017_retries_redirects_and_safe_body_decoding_are_wired(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("const HTTP_MAX_REDIRECTS: usize = 3", net)
        self.assertIn("const TCP_SYN_RETRIES: usize = 3", net)
        self.assertIn("const TCP_DATA_RETRIES: usize = 2", net)
        self.assertIn("TCP_RETRANSMIT_COUNT.fetch_add(1, Ordering::Relaxed)", net)
        self.assertIn("fn decode_chunked_body(body: &mut [u8])", net)
        self.assertIn("fn parse_content_length_header", net)
        self.assertIn("duplicate HTTP Content-Length headers are rejected", net)
        self.assertIn("HTTP response contains both Transfer-Encoding and Content-Length", net)
        self.assertIn("HTTP redirect limit exceeded", net)

    def test_relative_redirect_path_is_converted_to_bytes_before_append(self) -> None:
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("path[..directory_end].as_bytes()", net)
        self.assertNotIn("append_bytes(output, &mut used, &path[..directory_end])?", net)

    def test_fetch_is_bounded_user_scoped_and_never_saves_partial_responses(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        self.assertIn('"FETCH" =>', main)
        self.assertIn("const FETCH_RESPONSE_CAPACITY: usize = 16 * 1024", main)
        fetch_start = main.index('"FETCH" =>')
        fetch_end = main.index('"DHCP" =>', fetch_start)
        fetch = main[fetch_start:fetch_end]
        self.assertIn("vfs::disk_path(destination).is_none()", fetch)
        self.assertIn("users::may_access_path(destination)", fetch)
        self.assertIn("Ok(info) if info.truncated", fetch)
        self.assertLess(fetch.index("Ok(info) if info.truncated"), fetch.index("vfs::write_file(temporary_path, body)"))
        self.assertIn("200..300", fetch)

    def test_netstat_reports_http_and_tcp_retry_counters(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        for field in ("tcp_retransmits", "http_requests", "http_successes", "http_failures", "http_response_bytes"):
            self.assertIn(field, net)
        for label in ("TCP RETRANSMITS:", "HTTP REQUESTS:", "HTTP SUCCEEDED:", "HTTP FAILED:", "HTTP RESPONSE BYTES:"):
            self.assertIn(label, main)


    def test_persistent_configuration_is_wired_and_checksummed(self) -> None:
        config = (ROOT / "src" / "config.rs").read_text(encoding="utf-8")
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        net = (ROOT / "src" / "net.rs").read_text(encoding="utf-8")
        self.assertIn("XVCFG018", config)
        self.assertIn("CONFIG.A", config)
        self.assertIn("CONFIG.B", config)
        self.assertIn("fn checksum32", config)
        self.assertIn('"CONFIG" | "SETTINGS" => command_config', main)
        self.assertIn("CONFIG SET TIMEZONE", main)
        self.assertIn("CONFIG SET NETWORK", (ROOT / "README.md").read_text(encoding="utf-8"))
        self.assertIn("pub fn set_timeout_seconds", net)

    def test_fetch_commits_only_complete_payload_via_temp_rename(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        diskfs = (ROOT / "src" / "diskfs.rs").read_text(encoding="utf-8")
        vfs = (ROOT / "src" / "vfs.rs").read_text(encoding="utf-8")
        self.assertIn(".XVRFETCH.TMP", main)
        fetch_start = main.index('"FETCH" =>')
        fetch_end = main.index('"DHCP" =>', fetch_start)
        fetch = main[fetch_start:fetch_end]
        self.assertIn("if info.body_offset <= body_end && body_end <= response.len()", fetch)
        self.assertIn("vfs::write_file(temporary_path, body)", fetch)
        self.assertIn("vfs::rename_file(temporary_path, destination)", fetch)
        self.assertIn("DESTINATION ALREADY EXISTS; REFUSING TO OVERWRITE", diskfs)
        self.assertIn("pub fn rename_file(from: &str, to: &str)", vfs)

    def test_all_disk_paths_reject_traversal_before_privilege_shortcut(self) -> None:
        users = (ROOT / "src" / "users.rs").read_text(encoding="utf-8")
        validation = users.index("path.split('/').any(|part| part == \".\" || part == \"..\")")
        disk_shortcut = users.index('if vfs::disk_path(path).is_none() { return true; }', validation)
        self.assertLess(validation, disk_shortcut)

    def test_report_and_compact_configuration_docs_are_present(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn('"REPORT" => command_report', main)
        self.assertIn('"CONFIG" | "SETTINGS" => command_config', main)
        self.assertIn("REPORT SAVE", readme)
        self.assertIn("HTTPS/TLS", readme)

    def test_shell_does_not_override_saved_static_network_mode(self) -> None:
        main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        start = main.index("fn run_console(")
        end = main.index("fn redraw_input(", start)
        console = main[start:end]
        self.assertIn("let network_settings = config::current();", console)
        self.assertIn("if network_settings.use_dhcp", console)
        self.assertNotIn("net::acquire_dhcp()", console)

    def test_update_check_is_not_claimed_without_https_tls(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("Automatic GitHub release checking is not implemented yet", readme)
        self.assertIn("HTTPS/TLS support is required", readme)

if __name__ == "__main__":
    unittest.main(verbosity=2)

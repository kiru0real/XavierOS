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
            self.assertEqual(iso.name, "xavieros-0.1.4.iso")
            self.assertEqual(iso.read_bytes(), source.read_bytes())
            digest = package_release.sha256_file(iso)
            self.assertEqual(sums.read_text(encoding="ascii"), f"{digest}  {iso.name}\n")


class RepositoryShapeTests(unittest.TestCase):
    def test_version_metadata_is_consistent(self) -> None:
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        package = (ROOT / "package_release.py").read_text(encoding="utf-8")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn('version = "0.1.4"', cargo)
        self.assertIn('VERSION = "0.1.4"', package)
        self.assertIn("Version 0.1.4", readme)

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
        self.assertIn('sysinfo_row(console, "XX              XX", "XAVIEROS 0.1.4")', main)
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
        self.assertIn('VERSION: 0.1.4', main)

    def test_fsck_scans_without_writing(self) -> None:
        diskfs = (ROOT / "src" / "diskfs.rs").read_text(encoding="utf-8")
        start = diskfs.index("pub fn check_disk()")
        end = diskfs.index("pub fn format()", start)
        check_body = diskfs[start:end]
        self.assertIn("ata::read_sector", check_body)
        self.assertNotIn("ata::write_sector", check_body)
        self.assertIn("read-only metadata check", (ROOT / "README.md").read_text(encoding="utf-8"))

    def test_fsck_and_df_document_their_safety_boundary(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("does not repair", readme)
        self.assertIn("read-only metadata check", readme)
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


if __name__ == "__main__":
    unittest.main(verbosity=2)

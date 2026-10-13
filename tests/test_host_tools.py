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
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "Foo.txt").write_text("a", encoding="utf-8")
            (root / "foo.TXT").write_text("b", encoding="utf-8")
            with self.assertRaises(ValueError):
                mkinitramfs.build(root, Path(temp) / "out.xfs")

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
            self.assertEqual(iso.name, "xavieros-0.1.1.iso")
            self.assertEqual(iso.read_bytes(), source.read_bytes())
            digest = package_release.sha256_file(iso)
            self.assertEqual(sums.read_text(encoding="ascii"), f"{digest}  {iso.name}\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)

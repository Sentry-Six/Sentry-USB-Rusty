import importlib.util
from pathlib import Path
import stat
import struct
import tempfile
import unittest
import warnings
import zipfile


spec = importlib.util.spec_from_file_location(
    "extract_release_artifacts", Path(__file__).with_name("extract-release-artifacts.py")
)
extractor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(extractor)


class ReleaseExtractionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "downloads"
        self.output = self.root / "release"
        self.source.mkdir()
        self.bundles = extractor.expected_bundles()
        self.contents = {}
        for bundle, names in self.bundles.items():
            folder = self.source / bundle
            folder.mkdir()
            with zipfile.ZipFile(folder / "artifact", "w") as archive:
                for name in sorted(names):
                    data = b"\x7fELF\x00\xff" + name.encode()
                    self.contents[name] = data
                    archive.writestr(name, data)
        self.bundle = "sentryusb-linux-armv7-bundle"
        self.archive = self.source / self.bundle / "artifact"

    def rewrite(self, entries):
        with zipfile.ZipFile(self.archive, "w") as archive:
            for name, data in entries:
                archive.writestr(name, data)

    def entries(self):
        return [(name, self.contents[name]) for name in sorted(self.bundles[self.bundle])]

    def assert_rejected(self):
        with self.assertRaises((ValueError, OSError, zipfile.BadZipFile)):
            extractor.extract_release(self.source, self.output)
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.root.glob(".release-extract-*")), [])

    def test_all_fifteen_binaries_are_byte_exact_without_zip_extension(self):
        extractor.extract_release(self.source, self.output)
        self.assertEqual(len(list(self.output.iterdir())), 15)
        self.assertEqual(
            {entry.name: entry.read_bytes() for entry in self.output.iterdir()}, self.contents
        )

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        marker = self.output / "existing"
        marker.write_bytes(b"keep")
        with self.assertRaisesRegex(ValueError, "already exists"):
            extractor.extract_release(self.source, self.output)
        self.assertEqual(marker.read_bytes(), b"keep")
        self.assertEqual(list(self.output.iterdir()), [marker])

    def test_unsafe_and_unexpected_paths_are_rejected(self):
        for bad_name in ("../escape", "/tmp/escape", "folder/binary", "folder\\binary", "unexpected"):
            with self.subTest(name=bad_name):
                self.rewrite(self.entries() + [(bad_name, b"bad")])
                self.assert_rejected()
        self.assertFalse((self.root / "escape").exists())

    def test_symlink_entry_is_rejected_even_with_expected_name(self):
        entries = self.entries()
        entry = zipfile.ZipInfo(entries[0][0])
        entry.create_system = 3
        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
        self.rewrite([(entry, b"../elsewhere")] + entries[1:])
        self.assert_rejected()

    def test_duplicate_entry_is_rejected(self):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            self.rewrite(self.entries() + self.entries()[:1])
        self.assert_rejected()

    def test_missing_binary_is_rejected(self):
        self.rewrite(self.entries()[1:])
        self.assert_rejected()

    def test_binary_in_wrong_bundle_is_rejected(self):
        entries = self.entries()
        self.rewrite([(entries[0][0].replace("armv7", "arm64"), entries[0][1])] + entries[1:])
        self.assert_rejected()

    def test_crc_failure_removes_staging_and_publishes_nothing(self):
        raw = bytearray(self.archive.read_bytes())
        name_length, extra_length = struct.unpack_from("<HH", raw, 26)
        raw[30 + name_length + extra_length] ^= 0xFF
        self.archive.write_bytes(raw)
        self.assert_rejected()

    def test_invalid_archive_is_rejected(self):
        self.archive.write_bytes(b"not a ZIP")
        self.assert_rejected()

    def test_extra_archive_is_rejected(self):
        self.archive.with_name("second").write_bytes(self.archive.read_bytes())
        self.assert_rejected()

    def test_missing_and_unexpected_bundle_are_rejected(self):
        folder = self.archive.parent
        folder.rename(folder.with_name("unexpected-bundle"))
        self.assert_rejected()

    def test_archive_symlink_is_rejected(self):
        relocated = self.root / "archive"
        self.archive.rename(relocated)
        self.archive.symlink_to(relocated)
        self.assert_rejected()


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Architecture selection tests: no actual disks, network, or repairs."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import hashlib
import io
import os
import json
import subprocess
import sys
import time
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("repair_tool", Path(__file__).resolve().parents[1] / "run/xfs-repair-tool.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

def elf(bits, machine):
    b = bytearray(64)
    b[:4] = b"\x7fELF"
    b[4] = 2 if bits == 64 else 1
    b[5] = 1
    b[18:20] = machine.to_bytes(2, "little")
    return bytes(b)

def cached_fixture(root):
    files = {}
    for name in ('usr/sbin/xfs_repair', 'usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1'):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(elf(64, 183))
        files[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    manifest = {'packages': tool.PACKAGES, 'files': files}
    (root / 'verified.json').write_text(json.dumps(manifest))
    return manifest

class RepairToolTests(unittest.TestCase):
    def test_aarch64_kernel_rejects_armhf_even_with_32bit_userspace(self):
        self.assertFalse(tool.compatible("aarch64", elf(32, 40)))
        self.assertTrue(tool.compatible("aarch64", elf(64, 183)))

    def test_native_32bit_and_x86_are_supported(self):
        self.assertTrue(tool.compatible("armv7l", elf(32, 40)))
        self.assertFalse(tool.compatible("armv7l", elf(64, 183)))
        self.assertTrue(tool.compatible("x86_64", elf(64, 62)))
        self.assertTrue(tool.compatible("i686", elf(32, 3)))

    def test_scripts_truncation_and_wrong_machine_fail_closed(self):
        for data in (b"#!/bin/sh", b"\x7fELF", elf(64, 62)):
            self.assertFalse(tool.compatible("aarch64", data))
        self.assertFalse(tool.compatible("unknown", elf(64, 183)))

    def test_native_selection_does_not_download(self):
        with tempfile.TemporaryDirectory() as d:
            binary = Path(d) / "xfs_repair"
            with patch.object(tool, "prepare_bundle", side_effect=AssertionError("network")):
                for kernel, bits, machine in [('armv6l', 32, 40), ('armv7l', 32, 40),
                                               ('aarch64', 64, 183), ('x86_64', 64, 62),
                                               ('i686', 32, 3)]:
                    with self.subTest(kernel=kernel):
                        binary.write_bytes(elf(bits, machine))
                        self.assertEqual(tool.select_tool(kernel, binary, Path(d)), [str(binary)])

    def test_missing_native_tool_on_32bit_fails_without_downloading_arm64(self):
        with patch.object(tool, 'prepare_bundle', side_effect=AssertionError('wrong architecture')):
            with self.assertRaisesRegex(RuntimeError, 'install native xfsprogs'):
                tool.select_tool('armv7l', None, Path('/unused'))

    def test_mixed_arm_uses_bundle_and_never_falls_back(self):
        with tempfile.TemporaryDirectory() as d:
            binary = Path(d) / "xfs_repair"
            binary.write_bytes(elf(32, 40))
            with patch.object(tool, "prepare_bundle", return_value=["loader", "--library-path", "libs", "repair"]) as prepare:
                self.assertEqual(tool.select_tool("aarch64", binary, Path(d)), ["loader", "--library-path", "libs", "repair"])
                prepare.assert_called_once()
            with patch.object(tool, "prepare_bundle", side_effect=RuntimeError("offline")):
                with self.assertRaisesRegex(RuntimeError, "offline"):
                    tool.select_tool("aarch64", binary, Path(d))

    def test_download_checksum_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "checksum"):
            tool.verify_package(b"bad package", "0" * 64)

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_bundle_is_verified_cached_offline_and_corruption_is_rebuilt(self):
        data = b'verified deb fixture'
        packages = [['pool/fixture.deb', hashlib.sha256(data).hexdigest()]]
        def run(args, **kwargs):
            if args[0] == 'dpkg-deb':
                root = Path(args[-1])
                binary = root / 'usr/sbin/xfs_repair'
                loader = root / 'usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1'
                for f in (binary, loader):
                    f.parent.mkdir(parents=True, exist_ok=True)
                    f.write_bytes(elf(64, 183))
            else:
                self.assertEqual(args[-1], '-V')
        with tempfile.TemporaryDirectory() as d, patch.object(tool, 'PACKAGES', packages):
            with patch.object(tool.urllib.request, 'urlopen', return_value=io.BytesIO(data)), patch.object(tool.subprocess, 'run', side_effect=run):
                command = tool.prepare_bundle(Path(d))
            with patch.object(tool.urllib.request, 'urlopen', side_effect=AssertionError('offline')):
                self.assertEqual(tool.prepare_bundle(Path(d)), command)
                # Existing installs boot read-only. A valid persistent cache
                # must work without opening its write lock or using /run.
                volatile = Path(d) / 'unused-volatile'
                with patch.object(tool, 'PERSISTENT', Path(d)), patch.object(tool, 'VOLATILE', volatile), \
                     patch.object(tool.platform, 'machine', return_value='aarch64'), \
                     patch.object(tool.shutil, 'which', return_value=None), \
                     patch.object(tool.os, 'access', return_value=False), \
                     patch.object(tool.sys, 'stdout', new_callable=io.StringIO) as stdout:
                    tool.main()
                    self.assertEqual(json.loads(stdout.getvalue()), command)
                    self.assertFalse(volatile.exists())
                Path(command[-1]).write_bytes(elf(32, 40))
                self.assertFalse(tool.bundle_valid(Path(command[-1]).parents[2]))
            # An invalid writable cache must not permanently block repair.
            # Preserve the damaged tree for inspection, then rebuild only
            # from freshly checksum-verified packages.
            with patch.object(tool.urllib.request, 'urlopen', return_value=io.BytesIO(data)), patch.object(tool.subprocess, 'run', side_effect=run):
                self.assertEqual(tool.prepare_bundle(Path(d)), command)
            self.assertTrue(tool.bundle_valid(Path(command[-1]).parents[2]))
            quarantined = list(Path(d).glob('.bad-*'))
            self.assertEqual(len(quarantined), 1)
            self.assertEqual((quarantined[0] / 'usr/sbin/xfs_repair').read_bytes(), elf(32, 40))

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_corrupt_cache_with_failed_download_is_preserved_and_can_retry(self):
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d)
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            root.mkdir()
            (root / 'damaged').write_bytes(b'keep this evidence')
            with patch.object(tool.urllib.request, 'urlopen', side_effect=RuntimeError('offline')):
                with self.assertRaisesRegex(RuntimeError, 'offline'):
                    tool.prepare_bundle(cache)
            self.assertFalse(root.exists(), 'invalid tree must not occupy the active cache path')
            quarantined = list(cache.glob('.bad-*'))
            self.assertEqual(len(quarantined), 1)
            self.assertEqual((quarantined[0] / 'damaged').read_bytes(), b'keep this evidence')
            self.assertEqual(list(cache.glob('.prepare-*')), [])

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_new_quarantine_replaces_older_copies_even_when_rebuild_fails(self):
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d)
            old = cache / '.bad-previous'
            old.mkdir()
            (old / 'evidence').write_bytes(b'older damage')
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            root.mkdir()
            (root / 'evidence').write_bytes(b'latest damage')
            # A rename keeps the directory's old mtime. Retention must keep
            # the newly quarantined tree, not choose by its original mtime.
            os.utime(root, (1, 1))
            with patch.object(tool.urllib.request, 'urlopen', side_effect=RuntimeError('offline')):
                with self.assertRaisesRegex(RuntimeError, 'offline'):
                    tool.prepare_bundle(cache)
            bad = list(cache.glob('.bad-*'))
            self.assertEqual(len(bad), 1)
            self.assertEqual((bad[0] / 'evidence').read_bytes(), b'latest damage')
            self.assertFalse(old.exists())
            self.assertFalse(root.exists())

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking and symlinks')
    def test_writable_cache_reuse_prunes_old_quarantine_without_following_symlinks(self):
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d) / 'cache'
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            cached_fixture(root)
            outside = Path(d) / 'preserved'
            outside.mkdir()
            (outside / 'evidence').write_bytes(b'not cache data')
            old = cache / '.bad-old-link'
            old.symlink_to(outside, target_is_directory=True)
            time.sleep(0.01)
            newest = cache / '.bad-newest'
            newest.mkdir()
            (newest / 'evidence').write_bytes(b'latest quarantine')
            self.assertGreater(newest.stat().st_ctime_ns, old.lstat().st_ctime_ns)
            with patch.object(tool, 'PERSISTENT', cache), patch.object(tool, 'VOLATILE', Path(d) / 'unused'), \
                 patch.object(tool.platform, 'machine', return_value='aarch64'), \
                 patch.object(tool.shutil, 'which', return_value=None), \
                 patch.object(tool.urllib.request, 'urlopen', side_effect=AssertionError('network')), \
                 patch.object(tool.sys, 'stdout', new_callable=io.StringIO) as stdout:
                tool.main()
                self.assertEqual(json.loads(stdout.getvalue()), tool.bundle_command(root))
            self.assertEqual(list(cache.glob('.bad-*')), [newest])
            self.assertEqual((outside / 'evidence').read_bytes(), b'not cache data')
            self.assertTrue(tool.bundle_valid(root))

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_quarantine_cleanup_failure_does_not_block_a_valid_tool(self):
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d)
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            cached_fixture(root)
            (cache / '.bad-old').mkdir()
            (cache / '.bad-new').mkdir()
            with patch.object(tool.shutil, 'rmtree', side_effect=PermissionError('read-only entry')), \
                 patch.object(tool.urllib.request, 'urlopen', side_effect=AssertionError('network')):
                self.assertEqual(tool.prepare_bundle(cache), tool.bundle_command(root))
            self.assertTrue(tool.bundle_valid(root))

    def test_cache_inventory_rejects_missing_extra_or_malformed_records(self):
        for case in ('empty', 'omitted', 'extra-library', 'list', 'wrong-top-level', 'escape'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as d:
                root = Path(d) / 'bundle'
                manifest = cached_fixture(root)
                self.assertTrue(tool.bundle_valid(root))
                if case == 'empty':
                    manifest['files'] = {}
                elif case == 'omitted':
                    del manifest['files']['usr/sbin/xfs_repair']
                elif case == 'extra-library':
                    (root / 'usr/lib/aarch64-linux-gnu/libunexpected.so').write_bytes(b'extra')
                elif case == 'list':
                    manifest['files'] = []
                elif case == 'wrong-top-level':
                    manifest = []
                elif case == 'escape':
                    outside = Path(d) / 'outside'
                    outside.write_bytes(b'outside')
                    manifest['files']['../outside'] = hashlib.sha256(b'outside').hexdigest()
                (root / 'verified.json').write_text(json.dumps(manifest))
                self.assertFalse(tool.bundle_valid(root), f'accepted {case} inventory')

    @unittest.skipIf(os.name == 'nt', 'Linux symlinks')
    def test_cache_rejects_library_symlink_outside_private_tree(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / 'bundle'
            manifest = cached_fixture(root)
            outside = Path(d) / 'outside.so'
            outside.write_bytes(b'outside library')
            relative = 'usr/lib/aarch64-linux-gnu/liboutside.so'
            (root / relative).symlink_to(outside)
            manifest['files'][relative] = hashlib.sha256(outside.read_bytes()).hexdigest()
            (root / 'verified.json').write_text(json.dumps(manifest))
            self.assertFalse(tool.bundle_valid(root))

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_next_preparation_removes_only_abandoned_staging_entries(self):
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d)
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            cached_fixture(root)
            stale = cache / '.prepare-interrupted/root'
            stale.mkdir(parents=True)
            (stale / 'partial').write_bytes(b'partial extraction')
            preserved = cache / '.bad-previous'
            preserved.mkdir()
            (preserved / 'evidence').write_bytes(b'keep')
            (cache / '.prepare-symlink').symlink_to(preserved, target_is_directory=True)
            with patch.object(tool.urllib.request, 'urlopen', side_effect=AssertionError('network')):
                self.assertEqual(tool.prepare_bundle(cache), tool.bundle_command(root))
            self.assertEqual(list(cache.glob('.prepare-*')), [])
            self.assertEqual((preserved / 'evidence').read_bytes(), b'keep')

    @unittest.skipIf(os.name == 'nt', 'Linux process/lock inheritance')
    def test_extractor_holds_lock_after_preparer_is_killed(self):
        import fcntl
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d) / 'cache'
            ready, release = Path(d) / 'ready', Path(d) / 'release'
            extractor = f'''import pathlib, time
pathlib.Path({str(ready)!r}).touch()
deadline = time.monotonic() + 10
while not pathlib.Path({str(release)!r}).exists() and time.monotonic() < deadline:
    time.sleep(0.01)
'''
            probe = f'''import importlib.util, pathlib, hashlib, io, subprocess, sys
spec = importlib.util.spec_from_file_location('repair_tool', {str(Path(tool.__file__).resolve())!r})
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)
tool.PACKAGES = [['fixture.deb', hashlib.sha256(b'fixture').hexdigest()]]
tool.urllib.request.urlopen = lambda *a, **k: io.BytesIO(b'fixture')
run = subprocess.run
def extract(args, **kwargs):
    return run([sys.executable, '-c', {extractor!r}], **kwargs)
tool.subprocess.run = extract
tool.prepare_bundle(pathlib.Path({str(cache)!r}))
'''
            parent = subprocess.Popen([sys.executable, '-B', '-c', probe],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and parent.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), 'extractor did not start')
                parent.kill()
                parent.wait(timeout=5)
                with (cache / 'prepare.lock').open('a') as lock:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                release.touch()
                if parent.poll() is None:
                    parent.kill()
                parent.wait(timeout=5)
                # Wait for the orphan to release its inherited lock before
                # TemporaryDirectory removes the disposable staging tree.
                unlock = f"import fcntl; f=open({str(cache / 'prepare.lock')!r}, 'a'); fcntl.flock(f, fcntl.LOCK_EX)"
                subprocess.run([sys.executable, '-c', unlock], check=True, timeout=5)

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_bad_download_never_extracts_or_publishes(self):
        with tempfile.TemporaryDirectory() as d, patch.object(tool, 'PACKAGES', [['pool/bad.deb', '0' * 64]]):
            with patch.object(tool.urllib.request, 'urlopen', return_value=io.BytesIO(b'bad')), patch.object(tool.subprocess, 'run') as run:
                with self.assertRaisesRegex(RuntimeError, 'checksum'):
                    tool.prepare_bundle(Path(d))
                run.assert_not_called()
                self.assertEqual([f.name for f in Path(d).iterdir()], ['prepare.lock'])

    @unittest.skipIf(os.name == 'nt', 'Linux cache locking')
    def test_repair_waits_for_upgrade_preparation_and_reuses_its_cache(self):
        import fcntl
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d)
            version = hashlib.sha256(json.dumps(tool.PACKAGES).encode()).hexdigest()[:16]
            root = cache / ('arm64-' + version)
            files = {}
            for name in ('usr/sbin/xfs_repair', 'usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1'):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(elf(64, 183))
                files[name] = hashlib.sha256(path.read_bytes()).hexdigest()
            (root / 'verified.json').write_text(json.dumps({'packages': tool.PACKAGES, 'files': files}))
            self.assertTrue(tool.bundle_valid(root))
            probe = f'''import importlib.util, pathlib, json
spec = importlib.util.spec_from_file_location('repair_tool', {str(Path(tool.__file__).resolve())!r})
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)
print('ready', flush=True)
print(json.dumps(tool.prepare_bundle(pathlib.Path({d!r}))), flush=True)
'''
            with (cache / 'prepare.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                child = subprocess.Popen([sys.executable, '-B', '-c', probe],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                try:
                    self.assertEqual(child.stdout.readline().strip(), 'ready')
                    with self.assertRaises(subprocess.TimeoutExpired):
                        child.communicate(timeout=0.2)
                    fcntl.flock(lock, fcntl.LOCK_UN)
                    output, error = child.communicate(timeout=5)
                    self.assertEqual(child.returncode, 0, error)
                    self.assertEqual(json.loads(output), tool.bundle_command(root))
                finally:
                    fcntl.flock(lock, fcntl.LOCK_UN)
                    if child.poll() is None:
                        child.kill()
                    child.communicate()

if __name__ == "__main__":
    unittest.main()

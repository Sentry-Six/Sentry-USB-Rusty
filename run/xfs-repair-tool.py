#!/usr/bin/env python3
"""Select XFS repair by kernel/ELF ABI, including arm64 kernel + armhf userland.

Only prepares a command; never opens a disk. Debian packages are extracted, not
installed, and their pinned SHA256s are checked before extraction. No maintainer
scripts run and no system libraries are replaced. Keep this embedded in the API
so binary-only updates also get the selector.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import uuid

# Reviewed Debian trixie arm64 packages. Refresh versions and hashes together.
PACKAGES = [
    [
        "pool/main/g/glibc/libc6_2.41-12+deb13u4_arm64.deb",
        "8784eda966b189c777a384dac5ce009e8fc9b52d006926c5a013e7fa8aa688cc"
    ],
    [
        "pool/main/libi/libinih/libinih1_59-1_arm64.deb",
        "fcbc936af8fdd3db56b2a8fc4891b27955ce48c7639cbd6905fd292ae20061fa"
    ],
    [
        "pool/main/libu/liburcu/liburcu8t64_0.15.2-2_arm64.deb",
        "5901533f412afb4f5b89ce88c9b744062678db3d7436f14ce20a3bf8030c2129"
    ],
    [
        "pool/main/u/util-linux/libblkid1_2.41.5-0+deb13u1_arm64.deb",
        "d7d122b4c7a0a8ae6135a1687c9c4ce80a88f9e498c01998f634dd32caab9016"
    ],
    [
        "pool/main/u/util-linux/libuuid1_2.41.5-0+deb13u1_arm64.deb",
        "448ad6b190184dbff24508ef2eda88bd737a5ffc0f9750eb779fcf9885292bea"
    ],
    [
        "pool/main/x/xfsprogs/xfsprogs_6.13.0-2+deb13u1_arm64.deb",
        "4e5b910bd626cee9f91ce4948c6b60d8349cfe3ea0af3774045fbf35cf956e08"
    ]
]
PERSISTENT = Path('/opt/sentryusb/repair-tools')
VOLATILE = Path('/run/sentryusb-repair-tools')


def compatible(kernel, header):
    expected = {'aarch64': (2, 183), 'arm64': (2, 183),
                'armv7l': (1, 40), 'armv6l': (1, 40),
                'x86_64': (2, 62), 'i686': (1, 3), 'i386': (1, 3)}.get(kernel)
    return (expected is not None and len(header) >= 20
            and header[:4] == b'\x7fELF' and header[5] == 1
            and (header[4], int.from_bytes(header[18:20], 'little')) == expected)


def header(path):
    try:
        with open(path, 'rb') as f:
            return f.read(64)
    except OSError:
        return b''


def verify_package(data, digest):
    if hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError('XFS tool package checksum mismatch; refusing extraction')


def bundle_command(root):
    lib = root / 'usr/lib/aarch64-linux-gnu'
    return [str(lib / 'ld-linux-aarch64.so.1'), '--library-path', str(lib),
            str(root / 'usr/sbin/xfs_repair')]


def bundle_valid(root):
    try:
        if root.is_symlink() or (root / 'verified.json').is_symlink():
            return False
        manifest = json.loads((root / 'verified.json').read_text())
        if not isinstance(manifest, dict) or manifest.get('packages') != PACKAGES:
            return False
        files = manifest.get('files')
        if not isinstance(files, dict) or not files:
            return False
        actual = {str(f.relative_to(root)): f for f in root.rglob('*')
                  if (not f.is_dir() or f.is_symlink()) and f != root / 'verified.json'}
        if set(files) != set(actual):
            return False
        private_root = root.resolve()
        for name, path in actual.items():
            if not path.is_file():
                return False
            # Libraries must stay inside the private tree, including symlinks.
            path.resolve(strict=True).relative_to(private_root)
            if hashlib.sha256(path.read_bytes()).hexdigest() != files[name]:
                return False
        return (compatible('aarch64', header(root / 'usr/sbin/xfs_repair'))
                and compatible('aarch64', header(Path(bundle_command(root)[0]))))
    except (OSError, ValueError, KeyError):
        return False


def prune_quarantines(cache, keep=None):
    """Keep one damaged copy; caller must hold the preparation lock."""
    copies = list(cache.glob('.bad-*'))
    if not copies:
        return
    try:
        # Renaming a damaged tree retains its old mtime. A newly quarantined
        # copy is passed explicitly; older copies use inode-change time.
        newest = keep or max(copies, key=lambda path: (path.lstat().st_ctime_ns, path.name))
        for old in copies:
            if old == newest:
                continue
            if old.is_symlink() or not old.is_dir():
                old.unlink()
            else:
                shutil.rmtree(old)
    except OSError as error:
        # Housekeeping must not make an otherwise usable repair tool fail.
        print(f'Could not prune older XFS repair quarantines: {error}', file=sys.stderr)


def prepare_bundle(cache):
    import fcntl
    cache.mkdir(parents=True, exist_ok=True)
    # Separate versioned directories leave a running repair's libraries intact.
    version = hashlib.sha256(json.dumps(PACKAGES).encode()).hexdigest()[:16]
    root = cache / ('arm64-' + version)
    with open(cache / 'prepare.lock', 'a') as lock:
        # An upgrade can be preparing this cache when boot auto-repair starts.
        # Wait and reuse its result instead of failing the one-shot repair.
        # API, setup and upgrade callers all bound this process with a timeout.
        fcntl.flock(lock, fcntl.LOCK_EX)
        # A killed preparer cannot run finally. Extractors inherit this lock,
        # so no still-running child can be writing these trees during cleanup.
        for stale in cache.glob('.prepare-*'):
            if stale.is_symlink() or not stale.is_dir():
                stale.unlink()
            else:
                shutil.rmtree(stale)
        if bundle_valid(root):
            prune_quarantines(cache)
            return bundle_command(root)
        newest_bad = None
        if root.exists() or root.is_symlink():
            # Preserve evidence without leaving a broken tree at the active
            # cache path. A failed download can then be retried normally.
            bad = cache / ('.bad-' + root.name + '-' + uuid.uuid4().hex)
            root.rename(bad)
            newest_bad = bad
            print(f'Quarantined invalid XFS repair cache: {bad}; rebuilding', file=sys.stderr)
        prune_quarantines(cache, newest_bad)
        stage = Path(tempfile.mkdtemp(prefix='.prepare-', dir=cache))
        try:
            tree = stage / 'root'
            for filename, digest in PACKAGES:
                url = 'https://deb.debian.org/debian/' + filename
                with urllib.request.urlopen(url, timeout=30) as response:
                    data = response.read(16 * 1024 * 1024 + 1)
                verify_package(data, digest)
                deb = stage / Path(filename).name
                deb.write_bytes(data)
                subprocess.run(['dpkg-deb', '-x', str(deb), str(tree)],
                               check=True, timeout=30, stdout=subprocess.DEVNULL,
                               pass_fds=(lock.fileno(),))
            files = {str(f.relative_to(tree)): hashlib.sha256(f.read_bytes()).hexdigest()
                     for f in tree.rglob('*') if f.is_file()}
            (tree / 'verified.json').write_text(json.dumps({'packages': PACKAGES, 'files': files}))
            if not bundle_valid(tree):
                raise RuntimeError('Prepared XFS tool has an unexpected executable architecture')
            # Validate loader and shared libraries before publishing the cache.
            subprocess.run(bundle_command(tree) + ['-V'], check=True, timeout=10,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                           pass_fds=(lock.fileno(),))
            tree.rename(root)
        finally:
            shutil.rmtree(stage)
    return bundle_command(root)


def select_tool(kernel, native, cache):
    if native and compatible(kernel, header(native)):
        return [str(native)]
    if kernel in ('aarch64', 'arm64'):
        return prepare_bundle(cache)
    raise RuntimeError(f'No compatible XFS repair executable for kernel {kernel}; install native xfsprogs')


def main():
    kernel = platform.machine()
    native = shutil.which('xfs_repair', path='/usr/sbin:/sbin:/usr/bin:/bin')
    # Reuse installer-prepared tools offline. On read-only roots, downloads go
    # to /run, never the SSD partition that is about to be repaired.
    cache = PERSISTENT if os.access(PERSISTENT, os.W_OK) else VOLATILE
    if kernel in ('aarch64', 'arm64') and (not native or not compatible(kernel, header(native))):
        version = hashlib.sha256(json.dumps(PACKAGES).encode()).hexdigest()[:16]
        root = PERSISTENT / ('arm64-' + version)
        if bundle_valid(root):
            # Writable reuse also performs bounded quarantine housekeeping.
            # Read-only recovery must keep working without opening a lock.
            command = prepare_bundle(PERSISTENT) if cache == PERSISTENT else bundle_command(root)
            print(json.dumps(command))
            return
    if '--cache-persistent' in sys.argv:
        cache = PERSISTENT
    print(json.dumps(select_tool(kernel, native, cache)))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'Cannot prepare compatible XFS repair tool: {error}. '
              'No repair was started. Connect to the internet to prepare the arm64 tool, '
              'or provision its cache during installation.', file=sys.stderr)
        sys.exit(1)

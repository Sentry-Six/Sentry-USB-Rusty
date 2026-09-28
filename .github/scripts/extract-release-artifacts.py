#!/usr/bin/env python3
"""Extract the four digest-verified release bundles without Node's ZIP reader."""

import argparse
import os
from pathlib import Path
import shutil
import stat
import tempfile
import zipfile


BINARIES = ("sentryusb", "sentryusb-tesla-telemetry", "sentryusb-ble-action")
SUFFIXES = ("linux-arm64-a53", "linux-arm64-a72", "linux-arm64-a76", "linux-armv7")


def expected_bundles():
    bundles = {}
    for suffix in SUFFIXES:
        names = {f"{binary}-{suffix}" for binary in BINARIES}
        if suffix == "linux-arm64-a72":
            names.update(f"{binary}-linux-arm64" for binary in BINARIES)
        bundles[f"sentryusb-{suffix}-bundle"] = names
    return bundles


def extract_release(source, destination):
    source, destination = Path(source), Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError(f"Output already exists: {destination}")
    if source.is_symlink() or not source.is_dir():
        raise ValueError("Artifact staging path must be a directory")
    expected = expected_bundles()
    if {entry.name for entry in source.iterdir()} != set(expected):
        raise ValueError("Expected exactly the four release artifact directories")

    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".release-extract-", dir=destination.parent))
    try:
        for bundle, names in expected.items():
            folder = source / bundle
            if folder.is_symlink() or not folder.is_dir():
                raise ValueError(f"Artifact must be a directory: {bundle}")
            files = list(folder.iterdir())
            if len(files) != 1 or files[0].is_symlink() or not files[0].is_file():
                raise ValueError(f"Expected one archive in {bundle}")
            # Raw downloads use a response-header filename, sometimes just 'artifact'.
            with zipfile.ZipFile(files[0]) as archive:
                entries = archive.infolist()
                archive_names = [entry.filename for entry in entries]
                if len(archive_names) != len(set(archive_names)):
                    raise ValueError(f"Duplicate archive entry in {bundle}")
                if set(archive_names) != names:
                    raise ValueError(f"Missing or unexpected binary in {bundle}")
                for entry in entries:
                    mode = stat.S_IFMT(entry.external_attr >> 16)
                    if entry.is_dir() or mode not in (0, stat.S_IFREG):
                        raise ValueError(f"Archive entry is not a regular file: {entry.filename}")
                    with archive.open(entry) as src, (staging / entry.filename).open("xb") as dst:
                        shutil.copyfileobj(src, dst)

        # Publish only after every file has passed ZIP/CRC validation.
        if destination.exists() or destination.is_symlink():
            raise ValueError(f"Output already exists: {destination}")
        os.rename(staging, destination)
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        extract_release(args.source, args.destination)
    except (OSError, ValueError, RuntimeError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Release artifact extraction failed: {error}\n")
    print(f"Validated and extracted {sum(map(len, expected_bundles().values()))} release binaries")


if __name__ == "__main__":
    main()

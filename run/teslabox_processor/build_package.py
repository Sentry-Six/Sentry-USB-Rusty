#!/usr/bin/env python3
"""Build a checksum-covered, offline SentryUSB event processor archive."""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import tarfile
import tempfile
from pathlib import Path

SOURCE = Path(__file__).resolve().parent
APP_FILES = ("processor.py", "service.py", "requirements.lock")
UNIT_FILES = (
    "teslabox-capture.service",
    "teslabox-alert.service",
    "teslabox-processor.service",
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build(wheelhouse: Path, output: Path) -> tuple[str, int]:
    wheels = sorted(wheelhouse.glob("*.whl"))
    if not wheels:
        raise ValueError("wheelhouse contains no wheels")
    for source in [
        *(SOURCE / name for name in APP_FILES),
        *(SOURCE / name for name in UNIT_FILES),
        SOURCE / "install.sh",
        SOURCE / "README.md",
    ]:
        if not source.is_file():
            raise FileNotFoundError(source)

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="teslabox-package-") as temporary:
        package = Path(temporary) / "teslabox-processor-package"
        app = package / "app"
        units = package / "units"
        bundled_wheels = package / "wheels"
        app.mkdir(parents=True)
        units.mkdir()
        bundled_wheels.mkdir()
        for name in APP_FILES:
            shutil.copy2(SOURCE / name, app / name)
        for name in UNIT_FILES:
            shutil.copy2(SOURCE / name, units / name)
        shutil.copy2(SOURCE / "install.sh", package / "install.sh")
        shutil.copy2(SOURCE / "README.md", package / "README.md")
        os.chmod(package / "install.sh", 0o700)
        for wheel in wheels:
            shutil.copy2(wheel, bundled_wheels / wheel.name)

        payloads = sorted(
            path for path in package.rglob("*")
            if path.is_file() and path.name != "MANIFEST.sha256"
        )
        manifest = "".join(
            f"{sha256(path)}  {path.relative_to(package)}\n" for path in payloads
        )
        (package / "MANIFEST.sha256").write_text(manifest, encoding="utf-8")
        temporary_output = output.with_name(f".{output.name}.partial")
        temporary_output.unlink(missing_ok=True)
        try:
            with tarfile.open(temporary_output, "w:gz") as archive:
                archive.add(package, arcname=package.name)
            os.chmod(temporary_output, 0o600)
            os.replace(temporary_output, output)
        finally:
            temporary_output.unlink(missing_ok=True)
    return sha256(output), len(payloads)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheelhouse", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    digest, files = build(args.wheelhouse, args.output)
    print(f"package_sha256={digest} manifest_files={files} bytes={args.output.stat().st_size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

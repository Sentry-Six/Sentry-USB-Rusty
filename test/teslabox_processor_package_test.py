import hashlib
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BUILDER = REPO / "run" / "teslabox_processor" / "build_package.py"


class PackageBuilderTests(unittest.TestCase):
    def test_public_installer_has_no_appliance_identity_defaults(self):
        installer = (REPO / "run" / "teslabox_processor" / "install.sh").read_text(encoding="utf-8")
        self.assertIn('EXPECTED_HOSTNAME=${EXPECTED_HOSTNAME:-}', installer)
        self.assertIn('EXPECTED_MACHINE_ID_SHA256=${EXPECTED_MACHINE_ID_SHA256:-}', installer)
        self.assertNotRegex(
            installer,
            r"EXPECTED_MACHINE_ID_SHA256=\$\{EXPECTED_MACHINE_ID_SHA256:-[0-9a-f]{64}\}",
        )
        self.assertIn('S3_PREFIX=${S3_PREFIX:-${TESLABOX_OBJECT_PREFIX:-}}', installer)
        self.assertIn('SENTRYUSB_VEHICLE_LABEL=${SENTRYUSB_VEHICLE_LABEL:-${TESLABOX_VEHICLE_LABEL:-Tesla}}', installer)

    def test_public_configuration_uses_standard_vehicle_label(self):
        installer = (REPO / "run" / "teslabox_processor" / "install.sh").read_text(encoding="utf-8")
        self.assertIn("SENTRYUSB_VEHICLE_LABEL", installer)
        self.assertIn("S3_PREFIX is required", installer)

    def test_installer_shebang_is_kernel_compatible(self):
        installer = REPO / "run" / "teslabox_processor" / "install.sh"
        self.assertEqual(installer.read_text(encoding="utf-8").splitlines()[0], "#!/bin/bash")

    def test_installer_defines_s3_destination_before_use(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        declaration = "S3_DEST=$BASE/secrets/s3.json"
        first_use = 'if [ -s "$S3_DEST" ]; then'
        self.assertIn(declaration, installer)
        self.assertIn(first_use, installer)
        self.assertLess(installer.index(declaration), installer.index(first_use))
        self.assertIn('elif [ -s "$S3_STAGING" ]; then', installer)

    def test_installer_repairs_interrupted_venv_install(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("import boto3, botocore.config, botocore.exceptions", installer)
        self.assertIn("--require-hashes --force-reinstall", installer)

    def test_capture_unit_allows_only_shared_lock_under_tmp(self):
        unit = (
            REPO
            / "run"
            / "teslabox_processor"
            / "teslabox-capture.service"
        ).read_text(encoding="utf-8")
        read_write = next(line for line in unit.splitlines() if line.startswith("ReadWritePaths="))
        self.assertIn("/tmp/sentryusb_gadget_cycle.lock", read_write.split())
        self.assertNotIn("/tmp", read_write.split())

    def test_shared_queue_directories_are_group_writable(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('-m 0770 "$BASE/inbox" "$BASE/alerts"', installer)
        self.assertNotIn('-m 0750 "$BASE/inbox" "$BASE/alerts"', installer)

    def test_capture_unit_has_narrow_copy_permissions(self):
        unit = (
            REPO
            / "run"
            / "teslabox_processor"
            / "teslabox-capture.service"
        ).read_text(encoding="utf-8")
        self.assertIn("User=root\nGroup=teslabox-processor\n", unit)
        self.assertIn("CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN", unit)
        self.assertIn("AmbientCapabilities=CAP_SYS_ADMIN CAP_CHOWN", unit)
        self.assertNotIn("CAP_DAC_OVERRIDE", unit)
        self.assertNotIn("CAP_DAC_READ_SEARCH", unit)

    def test_network_units_share_tmp_for_appliance_resolver(self):
        units = REPO / "run" / "teslabox_processor"
        for name in ("teslabox-alert.service", "teslabox-processor.service"):
            unit = (units / name).read_text(encoding="utf-8")
            self.assertIn("PrivateTmp=no", unit)
            self.assertNotIn("PrivateTmp=yes", unit)

    def test_installer_rolls_back_failed_activation_before_commit(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("rollback_install()", installer)
        self.assertIn("TRANSACTION_STARTED=false", installer)
        self.assertIn('if [ "$TRANSACTION_STARTED" = true ] && [ "$COMMITTED" != true ]', installer)
        commit = installer.rindex("COMMITTED=true")
        readiness = installer.rindex("systemctl --quiet is-active")
        self.assertGreater(commit, readiness)

    def test_disabled_features_remove_unneeded_service_credentials(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('rm -f "$S3_DEST"', installer)
        self.assertIn('rm -f "$BASE/secrets/pushover.json"', installer)

    def test_generated_s3_credential_is_random_and_cleaned_by_trap(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        self.assertNotIn("S3_SOURCE=/tmp/teslabox-standard-s3.json", installer)
        self.assertIn("mktemp", installer)
        self.assertIn("cleanup_sensitive", installer)
        self.assertIn("trap cleanup_sensitive EXIT HUP INT TERM", installer)

    def test_installer_reads_standard_sentryusb_variables_and_controls_units(self):
        installer = (
            REPO / "run" / "teslabox_processor" / "install.sh"
        ).read_text(encoding="utf-8")
        for name in (
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
            "S3_ENDPOINT_URL",
            "S3_BUCKET",
            "S3_PREFIX",
            "SENTRYUSB_INSTANT_ALERTS_ENABLED",
            "SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED",
        ):
            self.assertIn(name, installer)
        self.assertIn('if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ]', installer)
        self.assertIn('if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]', installer)
        self.assertIn("systemctl disable --now", installer)

    def test_all_units_load_vehicle_configuration(self):
        units = REPO / "run" / "teslabox_processor"
        for name in (
            "teslabox-alert.service",
            "teslabox-capture.service",
            "teslabox-processor.service",
        ):
            unit = (units / name).read_text(encoding="utf-8")
            self.assertIn(
                "EnvironmentFile=-/backingfiles/teslabox-processor/processor.env",
                unit,
            )

    def test_builder_creates_expected_layout_and_valid_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            wheelhouse = root / "wheels"
            wheelhouse.mkdir()
            (wheelhouse / "dummy-1-py3-none-any.whl").write_bytes(b"wheel")
            archive = root / "package.tar.gz"
            subprocess.run(
                [sys.executable, str(BUILDER), "--wheelhouse", str(wheelhouse), "--output", str(archive)],
                check=True,
            )
            with tarfile.open(archive, "r:gz") as package:
                def read_member(name: str) -> bytes:
                    handle = package.extractfile(name)
                    if handle is None:
                        raise AssertionError(f"missing regular file: {name}")
                    return handle.read()

                names = set(package.getnames())
                required = {
                    "teslabox-processor-package/README.md",
                    "teslabox-processor-package/install.sh",
                    "teslabox-processor-package/MANIFEST.sha256",
                    "teslabox-processor-package/app/processor.py",
                    "teslabox-processor-package/app/service.py",
                    "teslabox-processor-package/app/requirements.lock",
                    "teslabox-processor-package/units/teslabox-capture.service",
                    "teslabox-processor-package/units/teslabox-alert.service",
                    "teslabox-processor-package/units/teslabox-processor.service",
                    "teslabox-processor-package/wheels/dummy-1-py3-none-any.whl",
                }
                self.assertTrue(required <= names)
                manifest = read_member(
                    "teslabox-processor-package/MANIFEST.sha256"
                ).decode()
                for line in manifest.splitlines():
                    expected, relative = line.split("  ", 1)
                    payload = read_member(
                        f"teslabox-processor-package/{relative}"
                    )
                    self.assertEqual(hashlib.sha256(payload).hexdigest(), expected)


if __name__ == "__main__":
    unittest.main()

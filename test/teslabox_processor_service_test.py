#!/usr/bin/env python3
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "run" / "teslabox_processor"))

from service import build_uploader, load_json_secret, read_feature_flags, resolve_s3_configuration


class CredentialTests(unittest.TestCase):
    def test_feature_flags_default_enabled_and_parse_explicit_false(self):
        self.assertEqual(read_feature_flags({}), (True, True))
        self.assertEqual(
            read_feature_flags(
                {
                    "SENTRYUSB_INSTANT_ALERTS_ENABLED": "false",
                    "SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED": "FALSE",
                }
            ),
            (False, False),
        )

    def test_feature_flags_reject_ambiguous_values(self):
        with self.assertRaisesRegex(ValueError, "SENTRYUSB_INSTANT_ALERTS_ENABLED"):
            read_feature_flags({"SENTRYUSB_INSTANT_ALERTS_ENABLED": "yes"})

    def test_standard_sentryusb_s3_variables_override_legacy_json(self):
        legacy = {
            "endpoint": "https://old.invalid",
            "bucket": "old-bucket",
            "region": "old-region",
            "access_key": "old-access",
            "secret_key": "old-secret",
        }
        environment = {
            "S3_ENDPOINT_URL": "https://minio.example.net",
            "S3_BUCKET": "vehicle-events",
            "AWS_REGION": "eu-central-1",
            "AWS_ACCESS_KEY_ID": "standard-access",
            "AWS_SECRET_ACCESS_KEY": "standard-secret",
            "AWS_SESSION_TOKEN": "standard-session",
        }

        self.assertEqual(
            resolve_s3_configuration(legacy, environment),
            {
                "endpoint": "https://minio.example.net",
                "bucket": "vehicle-events",
                "region": "eu-central-1",
                "access_key": "standard-access",
                "secret_key": "standard-secret",
                "session_token": "standard-session",
            },
        )

    def test_standard_s3_configuration_rejects_unsafe_endpoint_and_bucket(self):
        base = {
            "S3_ENDPOINT_URL": "http://minio.example.net",
            "S3_BUCKET": "vehicle-events",
            "AWS_REGION": "eu-central-1",
            "AWS_ACCESS_KEY_ID": "access",
            "AWS_SECRET_ACCESS_KEY": "secret",
        }
        with self.assertRaisesRegex(ValueError, "HTTPS"):
            resolve_s3_configuration({}, base)
        with self.assertRaisesRegex(ValueError, "bucket"):
            resolve_s3_configuration(
                {},
                {
                    **base,
                    "S3_ENDPOINT_URL": "https://minio.example.net",
                    "S3_BUCKET": "../bad",
                },
            )

    def test_rejects_group_or_world_readable_secret(self):
        with tempfile.TemporaryDirectory() as temp:
            secret = Path(temp) / "secret.json"
            secret.write_text('{"token":"x"}', encoding="utf-8")
            secret.chmod(0o644)
            with self.assertRaises(PermissionError):
                load_json_secret(secret)

    def test_rejects_symlink_and_accepts_mode_600_regular_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            secret = root / "secret.json"
            secret.write_text('{"token":"x"}', encoding="utf-8")
            secret.chmod(0o600)
            link = root / "link.json"
            link.symlink_to(secret)
            with self.assertRaises(PermissionError):
                load_json_secret(link)
            self.assertEqual(load_json_secret(secret), {"token": "x"})

    def test_accepts_systemd_projected_mode_440_only_in_credential_directory(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            credential_dir = root / "credentials"
            credential_dir.mkdir()
            projected = credential_dir / "secret.json"
            projected.write_text('{"token":"x"}', encoding="utf-8")
            projected.chmod(0o440)
            outside = root / "outside.json"
            outside.write_text('{"token":"x"}', encoding="utf-8")
            outside.chmod(0o440)
            with patch.dict(os.environ, {"CREDENTIALS_DIRECTORY": str(credential_dir)}):
                self.assertEqual(load_json_secret(projected), {"token": "x"})
                with self.assertRaises(PermissionError):
                    load_json_secret(outside)

    def test_build_uploader_uses_configured_region_and_session_token(self):
        import types
        from unittest.mock import patch

        captured = {}
        boto3 = types.ModuleType("boto3")
        setattr(boto3, "client", lambda *args, **kwargs: captured.update(kwargs) or object())
        botocore = types.ModuleType("botocore")
        botocore_config = types.ModuleType("botocore.config")
        setattr(botocore_config, "Config", lambda **kwargs: kwargs)
        secret = {
            "endpoint": "https://s3.example.net",
            "bucket": "teslabox",
            "region": "eu-central-1",
            "access_key": "access",
            "secret_key": "secret",
            "session_token": "session",
        }
        with patch.dict(
            sys.modules,
            {"boto3": boto3, "botocore": botocore, "botocore.config": botocore_config},
        ):
            build_uploader(secret, {})
        self.assertEqual(captured["region_name"], "eu-central-1")
        self.assertEqual(captured["aws_session_token"], "session")


if __name__ == "__main__":
    unittest.main()

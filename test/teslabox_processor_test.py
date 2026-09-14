import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "run" / "teslabox_processor"))

from processor import (
    AlertWorker,
    CaptureEngine,
    EventWorker,
    LiveMount,
    PushoverClient,
    S3Uploader,
    SourceChanged,
    event_window,
    focus_camera,
    object_keys,
    discover_events,
    event_signature,
    select_recent_clips,
    vehicle_label,
)


class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for category in ("SentryClips", "SavedClips", "RecentClips", "TeslaTrackMode"):
            (self.root / category).mkdir(parents=True)

    def tearDown(self):
        self.temp.cleanup()

    def make_event(self, category, name, groups=2):
        event = self.root / category / name
        event.mkdir()
        (event / "event.json").write_text('{"camera":"5","timestamp":"2026-09-13T12:00:25"}')
        for group in range(groups):
            second = 10 + group * 20
            for camera in ("front", "back", "left_repeater", "right_repeater"):
                (event / f"2026-09-13_12-00-{second:02d}-{camera}.mp4").write_bytes(
                    f"{group}-{camera}".encode()
                )
        return event

    def test_discovers_only_sentry_and_saved_with_event_metadata(self):
        sentry = self.make_event("SentryClips", "2026-09-13_12-00-00")
        saved = self.make_event("SavedClips", "2026-09-13_12-01-00")
        self.make_event("RecentClips", "2026-09-13_12-02-00")
        self.make_event("TeslaTrackMode", "2026-09-13_12-03-00")
        incomplete = self.root / "SentryClips" / "2026-09-13_12-04-00"
        incomplete.mkdir()
        (incomplete / "clip-front.mp4").write_bytes(b"partial")

        found = discover_events(self.root)

        self.assertEqual(found, [saved, sentry])

    def test_selects_two_complete_groups_around_event_timestamp(self):
        event = self.make_event("SentryClips", "2026-09-13_12-00-00", groups=3)

        selected = select_recent_clips(event, group_count=2)

        self.assertEqual(len(selected), 8)
        self.assertTrue(all("12-00-50" not in path.name for path in selected))
        self.assertTrue(any("12-00-10" in path.name for path in selected))
        self.assertTrue(any("12-00-30" in path.name for path in selected))
        self.assertEqual(
            {path.name.rsplit("-", 1)[-1].removesuffix(".mp4") for path in selected},
            {"front", "back", "left_repeater", "right_repeater"},
        )

    def test_signature_changes_when_selected_content_changes(self):
        event = self.make_event("SentryClips", "2026-09-13_12-00-00")
        first = event_signature(event)
        target = select_recent_clips(event)[0]
        target.write_bytes(b"changed-video")
        os.utime(target, None)
        second = event_signature(event)
        self.assertNotEqual(first, second)


class CaptureTests(DiscoveryTests):
    def setUp(self):
        super().setUp()
        self.state = self.root / "state"
        self.inbox = self.root / "inbox"

    def exercise_feature_mode(self, *, instant_alerts_enabled, video_enabled):
        alerts = self.root / "alerts"
        engine = CaptureEngine(
            self.state,
            self.inbox,
            alerts=alerts,
            instant_alerts_enabled=instant_alerts_enabled,
            video_enabled=video_enabled,
        )
        engine.bootstrap([])
        event = self.make_event("SentryClips", "2026-09-13_12-20-00")
        self.assertEqual(engine.capture([event]), [])
        captured = engine.capture([event])
        engine.close()
        return captured, list(alerts.glob("*.json")), list(self.inbox.glob("*"))

    def test_instant_only_queues_alert_without_copying_video(self):
        captured, alerts, inbox = self.exercise_feature_mode(
            instant_alerts_enabled=True, video_enabled=False
        )
        self.assertEqual(captured, [])
        self.assertEqual(len(alerts), 1)
        self.assertEqual(inbox, [])

    def test_video_only_copies_video_without_queuing_instant_alert(self):
        captured, alerts, inbox = self.exercise_feature_mode(
            instant_alerts_enabled=False, video_enabled=True
        )
        self.assertEqual(len(captured), 1)
        self.assertEqual(alerts, [])
        self.assertEqual(inbox, captured)

    def test_disabled_features_record_event_without_queuing_work(self):
        captured, alerts, inbox = self.exercise_feature_mode(
            instant_alerts_enabled=False, video_enabled=False
        )
        self.assertEqual(captured, [])
        self.assertEqual(alerts, [])
        self.assertEqual(inbox, [])

    def test_bootstrap_ignores_history_then_atomically_captures_new_event(self):
        historical = self.make_event("SentryClips", "2026-09-13_12-00-00")
        engine = CaptureEngine(self.state, self.inbox)

        self.assertEqual(engine.bootstrap([historical]), 1)
        self.assertEqual(list(self.inbox.glob("*")), [])

        new_event = self.make_event("SentryClips", "2026-09-13_12-10-00")
        self.assertEqual(engine.capture([historical, new_event]), [])
        engine.close()
        engine = CaptureEngine(self.state, self.inbox)
        captured = engine.capture([historical, new_event])

        self.assertEqual(len(captured), 1)
        copied_names = {path.name for path in captured[0].iterdir()}
        self.assertIn("event.json", copied_names)
        self.assertEqual(len([name for name in copied_names if name.endswith(".mp4")]), 8)
        self.assertFalse(any("partial" in path.name for path in self.inbox.iterdir()))
        self.assertEqual(engine.capture([new_event]), [])
        engine.close()

    def test_source_change_during_copy_leaves_no_queue_entry(self):
        event = self.make_event("SentryClips", "2026-09-13_12-00-00")
        changed = False

        def mutating_copy(source, destination):
            nonlocal changed
            import shutil
            result = shutil.copy2(source, destination)
            if not changed and source.suffix == ".mp4":
                changed = True
                source.write_bytes(source.read_bytes() + b"changed")
                os.utime(source, None)
            return result

        engine = CaptureEngine(self.state, self.inbox, copy_file=mutating_copy)
        engine.bootstrap([])

        self.assertEqual(engine.capture([event]), [])
        with self.assertRaises(SourceChanged):
            engine.capture([event])

        self.assertEqual(list(self.inbox.glob("*")), [])
        engine.close()

    def test_alert_is_durable_before_video_copy_begins(self):
        import shutil

        alerts = self.root / "alerts"
        observed_alerts = []

        def checking_copy(source, destination):
            observed_alerts.append(list(alerts.glob("*.json")))
            return shutil.copy2(source, destination)

        engine = CaptureEngine(self.state, self.inbox, alerts=alerts, copy_file=checking_copy)
        engine.bootstrap([])
        event = self.make_event("SentryClips", "2026-09-13_12-20-00")
        self.assertEqual(engine.capture([event]), [])
        self.assertEqual(len(engine.capture([event])), 1)
        self.assertTrue(observed_alerts)
        self.assertTrue(all(len(markers) == 1 for markers in observed_alerts))
        marker = next(alerts.glob("*.json"))
        self.assertFalse(__import__("json").loads(marker.read_text())["sent"])
        engine.close()

    def test_set_owner_covers_nested_artifacts(self):
        from unittest.mock import patch

        engine = CaptureEngine(self.state, self.inbox, owner_uid=123, owner_gid=456)
        tree = self.root / "tree"
        nested = tree / "derived"
        nested.mkdir(parents=True)
        leaf = nested / "preview.jpg"
        leaf.write_bytes(b"preview")
        with patch("processor.os.chown") as chown:
            engine._set_owner(tree)
        self.assertEqual(
            {Path(call.args[0]) for call in chown.call_args_list},
            {tree, nested, leaf},
        )
        engine.close()


class LiveMountTests(unittest.TestCase):
    def test_every_scan_remounts_and_exception_still_unmounts(self):
        class Result:
            stdout = "exfat umask=000,offset=1048576,time_offset=-420\n"

        calls = []

        def runner(command, **kwargs):
            calls.append(command)
            return Result()

        lock_modes = []
        real_flock = __import__("fcntl").flock

        def recording_flock(fd, mode):
            lock_modes.append(mode)
            return real_flock(fd, mode)

        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temp, patch("processor.fcntl.flock", recording_flock):
            root = Path(temp)
            observer = LiveMount(
                root / "cam_disk.bin",
                root / "mount",
                root / "gadget.lock",
                runner=runner,
            )
            with self.assertRaises(RuntimeError):
                with observer.mounted():
                    raise RuntimeError("scan failed")
            with observer.mounted() as mountpoint:
                self.assertEqual(mountpoint, root / "mount")

        self.assertEqual(sum(command[0] == "mount" for command in calls), 2)
        self.assertEqual(sum(command[0] == "umount" for command in calls), 2)
        mount_command = next(command for command in calls if command[0] == "mount")
        option_string = mount_command[mount_command.index("-o") + 1]
        self.assertIn("ro,nodev,nosuid,noexec", option_string)
        self.assertEqual(lock_modes[0], __import__("fcntl").LOCK_EX)

    def test_unmount_failure_uses_private_namespace_lazy_detach(self):
        class Result:
            stdout = "exfat umask=000,offset=1048576,time_offset=-420\n"

        calls = []

        def runner(command, **kwargs):
            calls.append(command)
            if command[0] == "umount" and "-l" not in command:
                import subprocess
                raise subprocess.CalledProcessError(32, command)
            return Result()

        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            observer = LiveMount(
                root / "cam_disk.bin",
                root / "mount",
                root / "gadget.lock",
                runner=runner,
                unmount_retries=2,
            )
            with observer.mounted():
                pass

        normal = [command for command in calls if command[:1] == ["umount"] and "-l" not in command]
        lazy = [command for command in calls if command[:2] == ["umount", "-l"]]
        self.assertEqual(len(normal), 2)
        self.assertEqual(len(lazy), 1)


class ProcessingPlanTests(DiscoveryTests):
    def test_uses_ten_seconds_before_and_twenty_after_event(self):
        event = self.make_event("SentryClips", "2026-09-13_12-00-00")
        self.assertEqual(event_window(event), (5.0, 30.0))

    def test_camera_five_focuses_left_repeater(self):
        event = self.make_event("SentryClips", "2026-09-13_12-00-00")
        self.assertEqual(focus_camera(event), "left_repeater")

    def test_object_keys_use_neutral_non_s3_default(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            keys = object_keys("SentryClips/2026-09-13_12-00-00", "abc123")
        self.assertTrue(all(key.startswith("vehicle/") for key in keys.values()))

    def test_object_keys_remain_under_configured_prefix(self):
        with mock.patch.dict(os.environ, {"S3_PREFIX": "vehicle-name"}, clear=True):
            keys = object_keys("SentryClips/2026-09-13_12-00-00", "abc123")
        self.assertTrue(all(key.startswith("vehicle-name/") for key in keys.values()))

    def test_standard_vehicle_label_precedes_legacy_alias(self):
        with mock.patch.dict(
            os.environ,
            {"SENTRYUSB_VEHICLE_LABEL": "Family Tesla", "TESLABOX_VEHICLE_LABEL": "Legacy"},
            clear=True,
        ):
            self.assertEqual(vehicle_label(), "Family Tesla")

    def test_object_keys_support_exact_vehicle_prefix(self):
        with mock.patch.dict(os.environ, {"TESLABOX_OBJECT_PREFIX": "legacy-vehicle"}):
            keys = object_keys("SentryClips/2026-09-13_12-00-00", "abc123")
        self.assertTrue(all(key.startswith("legacy-vehicle/") for key in keys.values()))

    def test_s3_prefix_takes_precedence_over_legacy_prefix(self):
        with mock.patch.dict(
            os.environ,
            {"S3_PREFIX": "standard-prefix", "TESLABOX_OBJECT_PREFIX": "legacy-prefix"},
        ):
            self.assertEqual(
                object_keys("SentryClips/event", "abc")["video"],
                "standard-prefix/SentryClips/event/abc/composite.mp4",
            )


class AlertWorkerTests(unittest.TestCase):
    def setUp(self):
        self.environment = mock.patch.dict(
            os.environ, {"SENTRYUSB_VEHICLE_LABEL": "Family Tesla"}, clear=False
        )
        self.environment.start()

    def tearDown(self):
        self.environment.stop()

    def test_immediate_alert_is_sent_once_and_receipt_is_durable(self):
        import json

        calls = []

        class Notifier:
            def send(self, title, message, **kwargs):
                calls.append((title, message))

        with tempfile.TemporaryDirectory() as temp:
            marker = Path(temp) / "alert.json"
            marker.write_text(
                json.dumps(
                    {
                        "event_key": "SentryClips/2026-09-13_12-00-00",
                        "timestamp": "2026-09-13T12:00:25",
                        "sent": False,
                    }
                )
            )
            worker = AlertWorker(Notifier())
            self.assertTrue(worker.process(marker))
            self.assertFalse(worker.process(marker))
            self.assertTrue(json.loads(marker.read_text())["sent"])
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "Family Tesla Sentry event detected")


    def test_alert_failures_use_durable_backoff(self):
        import json

        calls = []
        now = [100.0]

        class OfflineNotifier:
            def send(self, title, message, **kwargs):
                calls.append(title)
                raise OSError("offline")

        with tempfile.TemporaryDirectory() as temp:
            marker = Path(temp) / "alert.json"
            marker.write_text(
                json.dumps(
                    {
                        "event_key": "SentryClips/2026-09-13_12-00-00",
                        "timestamp": "2026-09-13T12:00:25",
                        "sent": False,
                    }
                )
            )
            worker = AlertWorker(OfflineNotifier(), clock=lambda: now[0])
            with self.assertRaises(OSError):
                worker.process(marker)
            self.assertFalse(worker.process(marker))
            self.assertEqual(len(calls), 1)
            state = json.loads(marker.read_text())
            self.assertEqual(state["retry_after"], 110.0)
            now[0] = 110.0
            with self.assertRaises(OSError):
                worker.process(marker)
            self.assertEqual(len(calls), 2)


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.environment = mock.patch.dict(
            os.environ,
            {"S3_PREFIX": "vehicle-name", "SENTRYUSB_VEHICLE_LABEL": "Family Tesla"},
            clear=False,
        )
        self.environment.start()

    def tearDown(self):
        self.environment.stop()

    def make_queued_event(self, root):
        event = root / "SentryClips--2026-09-13_12-00-00--abc123"
        event.mkdir()
        (event / "event.json").write_text(
            '{"camera":"5","timestamp":"2026-09-13T12:00:25"}', encoding="utf-8"
        )
        (event / "capture.json").write_text(
            '{"event_key":"SentryClips/2026-09-13_12-00-00","signature":"abc123"}',
            encoding="utf-8",
        )
        return event

    def test_order_and_durable_idempotency(self):
        calls = []

        class Notifier:
            def send(self, title, message, **kwargs):
                calls.append(("notify", title, kwargs.get("url")))

        class Uploader:
            def upload(self, path, key, content_type):
                calls.append(("upload", key, content_type))

            def presign(self, key, expires):
                calls.append(("presign", key, expires))
                return "https://s3.example.net/presigned"

        def composer(event, video, preview):
            calls.append(("compose", event.name))
            video.parent.mkdir(exist_ok=True)
            video.write_bytes(b"video")
            preview.write_bytes(b"preview")

        with tempfile.TemporaryDirectory() as temp:
            event = self.make_queued_event(Path(temp))
            worker = EventWorker(Notifier(), Uploader(), composer=composer)
            self.assertTrue(worker.process(event))
            first_run = list(calls)
            self.assertTrue(worker.process(event))

        self.assertEqual(calls, first_run)
        self.assertEqual(calls[0][0:2], ("notify", "Family Tesla Sentry event detected"))
        self.assertEqual(calls[1][0], "compose")
        upload_keys = [call[1] for call in calls if call[0] == "upload"]
        self.assertEqual(
            upload_keys,
            [
                "vehicle-name/SentryClips/2026-09-13_12-00-00/abc123/composite.mp4",
                "vehicle-name/SentryClips/2026-09-13_12-00-00/abc123/preview.jpg",
                "vehicle-name/SentryClips/2026-09-13_12-00-00/abc123/event.json",
            ],
        )
        self.assertEqual(calls[-1], ("notify", "Family Tesla Sentry video ready", "https://s3.example.net/presigned"))

    def test_external_alert_receipt_prevents_delayed_duplicate(self):
        calls = []

        class Notifier:
            def send(self, title, message, **kwargs):
                calls.append(("notify", title))

        class Uploader:
            def upload(self, path, key, content_type):
                calls.append(("upload", key))

            def presign(self, key, expires):
                return "https://s3.example.net/presigned"

        def composer(event, video, preview):
            calls.append(("compose", event.name))
            video.parent.mkdir(exist_ok=True)
            video.write_bytes(b"video")
            preview.write_bytes(b"preview")

        with tempfile.TemporaryDirectory() as temp:
            event = self.make_queued_event(Path(temp))
            worker = EventWorker(Notifier(), Uploader(), composer=composer, send_detected=False)
            self.assertFalse(worker.process(event, detected_already_sent=False))
            self.assertFalse(any(call == ("notify", "Family Tesla Sentry event detected") for call in calls))
            self.assertTrue(worker.process(event, detected_already_sent=True))

        titles = [call[1] for call in calls if call[0] == "notify"]
        self.assertEqual(titles, ["Family Tesla Sentry video ready"])

    def test_notification_outage_does_not_block_local_processing(self):
        calls = []

        class OfflineNotifier:
            def send(self, *args, **kwargs):
                calls.append("notify-failed")
                raise OSError("offline")

        class OfflineUploader:
            def upload(self, *args, **kwargs):
                calls.append("upload-failed")
                raise OSError("offline")

            def presign(self, *args, **kwargs):
                raise AssertionError("not uploaded")

        def composer(event, video, preview):
            calls.append("compose")
            video.parent.mkdir(exist_ok=True)
            video.write_bytes(b"video")
            preview.write_bytes(b"preview")

        errors = []
        now = [100.0]
        with tempfile.TemporaryDirectory() as temp:
            event = self.make_queued_event(Path(temp))
            worker = EventWorker(
                OfflineNotifier(),
                OfflineUploader(),
                composer=composer,
                clock=lambda: now[0],
                on_error=lambda event, stage, error: errors.append(
                    (event.name, stage, type(error).__name__)
                ),
            )
            state = worker.process(event)
            first_attempt = list(calls)
            self.assertFalse(worker.process(event))
            self.assertEqual(calls, first_attempt)
            now[0] = 1000.0
            self.assertFalse(worker.process(event))

        self.assertFalse(state)
        self.assertIn("compose", calls)
        self.assertIn("upload-failed", calls)
        self.assertTrue(any(stage == "detected-notification" for _, stage, _ in errors))
        self.assertTrue(any(stage == "upload" for _, stage, _ in errors))


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.environment = mock.patch.dict(os.environ, {"S3_PREFIX": "vehicle-name"}, clear=False)
        self.environment.start()

    def tearDown(self):
        self.environment.stop()

    def test_pushover_posts_required_fields_and_optional_link(self):
        from urllib.parse import parse_qs

        requests = []

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            def read(self):
                return b'{"status":1}'

        def opener(request, timeout):
            requests.append((request, timeout))
            return Response()

        client = PushoverClient("app-token", "user-key", opener=opener)
        client.send(
            "Family Tesla Sentry video ready",
            "ready",
            url="https://s3.example.net/presigned",
        )

        request, timeout = requests[0]
        fields = parse_qs(request.data.decode())
        self.assertEqual(request.full_url, "https://api.pushover.net/1/messages.json")
        self.assertEqual(timeout, 20)
        self.assertEqual(fields["token"], ["app-token"])
        self.assertEqual(fields["user"], ["user-key"])
        self.assertEqual(fields["url"], ["https://s3.example.net/presigned"])

    def test_s3_wrapper_uses_teslabox_bucket_and_exact_key(self):
        calls = []

        class Client:
            def upload_file(self, source, bucket, key, ExtraArgs):
                calls.append(("upload", source, bucket, key, ExtraArgs))

            def generate_presigned_url(self, operation, Params, ExpiresIn):
                calls.append(("presign", operation, Params, ExpiresIn))
                return "https://s3.example.net/link"

        uploader = S3Uploader(Client(), bucket="example-bucket")
        uploader.upload(Path("/tmp/video.mp4"), "vehicle-name/SentryClips/event/rev/composite.mp4", "video/mp4")
        link = uploader.presign("vehicle-name/SentryClips/event/rev/composite.mp4", 604800)

        self.assertEqual(calls[0][2:4], ("example-bucket", "vehicle-name/SentryClips/event/rev/composite.mp4"))
        self.assertEqual(calls[1][2]["Bucket"], "example-bucket")
        self.assertEqual(calls[1][2]["Key"], "vehicle-name/SentryClips/event/rev/composite.mp4")
        self.assertEqual(link, "https://s3.example.net/link")

    def test_s3_wrapper_rejects_malformed_keys(self):
        class Client:
            def upload_file(self, *args, **kwargs):
                raise AssertionError("invalid key reached client")

            def generate_presigned_url(self, *args, **kwargs):
                raise AssertionError("invalid key reached client")

        uploader = S3Uploader(Client(), bucket="example-bucket")
        bad_keys = (
            "other-prefix/file.mp4",
            "vehicle-name/../file.mp4",
            "vehicle-name//file.mp4",
            "vehicle-name/SentryClips/./file.mp4",
            "vehicle-name/SentryClips/event\\file.mp4",
            "vehicle-name/SentryClips/event/line\nfeed.mp4",
        )
        for key in bad_keys:
            with self.subTest(key=repr(key)), self.assertRaises(ValueError):
                uploader.presign(key, 60)


if __name__ == "__main__":
    unittest.main()

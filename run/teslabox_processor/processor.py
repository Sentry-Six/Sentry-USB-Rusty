#!/usr/bin/env python3
"""Live Sentry event processing add-on for SentryUSB."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import tempfile
import time
import uuid
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Iterator, Protocol
from urllib.parse import urlencode
from urllib.request import Request, urlopen

ALLOWED_CATEGORIES = ("SavedClips", "SentryClips")
CAMERAS = ("front", "back", "left_repeater", "right_repeater")
CLIP_PATTERN = re.compile(
    r"^(?P<group>\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2})-(?P<camera>front|back|left_repeater|right_repeater)\.mp4$"
)


def object_prefix() -> str:
    prefix = (
        os.environ.get("S3_PREFIX")
        or os.environ.get("TESLABOX_OBJECT_PREFIX")
        or "vehicle"
    ).strip()
    if (
        len(prefix) > 80
        or "/" in prefix
        or "\\" in prefix
        or prefix in {".", ".."}
        or any(ord(character) < 32 for character in prefix)
    ):
        raise ValueError("invalid S3 object prefix")
    return prefix


def vehicle_label() -> str:
    label = (
        os.environ.get("SENTRYUSB_VEHICLE_LABEL")
        or os.environ.get("TESLABOX_VEHICLE_LABEL")
        or "Tesla"
    ).strip()
    if not label or len(label) > 80 or any(ord(character) < 32 for character in label):
        raise ValueError("invalid vehicle label")
    return label


def _event_time(event: Path) -> datetime:
    metadata = json.loads((event / "event.json").read_text(encoding="utf-8"))
    result = datetime.fromisoformat(str(metadata["timestamp"]).replace("Z", "+00:00"))
    return result.replace(tzinfo=None) if result.tzinfo is not None else result


def select_recent_clips(event: Path, group_count: int = 2) -> list[Path]:
    """Return complete camera groups centered on the event metadata timestamp."""
    if group_count < 1:
        raise ValueError("group_count must be positive")
    grouped: dict[str, dict[str, Path]] = {}
    for path in event.iterdir():
        if path.is_symlink() or not path.is_file():
            continue
        match = CLIP_PATTERN.match(path.name)
        if match:
            grouped.setdefault(match.group("group"), {})[match.group("camera")] = path
    complete = sorted(name for name, clips in grouped.items() if set(clips) == set(CAMERAS))
    if not complete:
        return []
    event_time = _event_time(event)
    starts = [datetime.strptime(name, "%Y-%m-%d_%H-%M-%S") for name in complete]
    preceding = max((index for index, start in enumerate(starts) if start <= event_time), default=0)
    first = min(preceding, max(0, len(complete) - group_count))
    chosen = set(complete[first : first + group_count])
    return sorted(path for name in chosen for path in grouped[name].values())


def discover_events(root: Path) -> list[Path]:
    """Discover complete live events, never RecentClips or Track Mode."""
    events: list[Path] = []
    for category in ALLOWED_CATEGORIES:
        parent = root / category
        if not parent.is_dir():
            continue
        for event in parent.iterdir():
            if event.is_symlink() or not event.is_dir():
                continue
            if not (event / "event.json").is_file():
                continue
            try:
                selected = select_recent_clips(event)
            except (OSError, ValueError, json.JSONDecodeError):
                continue
            if selected:
                events.append(event)
    return sorted(events)


def event_signature(event: Path) -> str:
    """Hash selected file identity and event metadata for idempotent capture."""
    digest = hashlib.sha256()
    files = [event / "event.json", *select_recent_clips(event)]
    for path in files:
        stat = path.stat()
        digest.update(path.name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(str(stat.st_size).encode())
        digest.update(b"\0")
        digest.update(str(stat.st_mtime_ns).encode())
        digest.update(b"\0")
        if path.name == "event.json":
            json.loads(path.read_text(encoding="utf-8"))
            digest.update(path.read_bytes())
    return digest.hexdigest()


def compose_event(
    event: Path,
    video: Path,
    preview: Path,
    *,
    runner: Callable[..., object] = subprocess.run,
) -> None:
    """Validate and compose four Tesla cameras into a compact 1280x720 video."""
    selected = select_recent_clips(event)
    clips_by_camera: dict[str, list[Path]] = {camera: [] for camera in CAMERAS}
    for path in selected:
        match = CLIP_PATTERN.match(path.name)
        if match:
            clips_by_camera[match.group("camera")].append(path)
    if any(len(clips_by_camera[camera]) < 1 for camera in CAMERAS):
        raise ValueError("four complete camera views are required")
    for clips in clips_by_camera.values():
        clips.sort()
        for clip in clips:
            runner(
                [
                    "ffprobe", "-v", "error", "-select_streams", "v:0",
                    "-show_entries", "stream=codec_name", "-of", "default=nw=1:nk=1",
                    str(clip),
                ],
                check=True,
                capture_output=True,
                text=True,
            )

    focus = focus_camera(event)
    camera_order = [focus, *[camera for camera in CAMERAS if camera != focus]]
    start, duration = event_window(event)
    video.parent.mkdir(parents=True, exist_ok=True)
    preview.parent.mkdir(parents=True, exist_ok=True)
    partial_video = video.with_name(f".{video.stem}.partial.mp4")
    partial_preview = preview.with_name(f".{preview.stem}.partial.jpg")

    try:
        with tempfile.TemporaryDirectory(prefix="teslabox-concat-", dir=video.parent) as temp:
            temp_root = Path(temp)
            command = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
            for camera in camera_order:
                concat = temp_root / f"{camera}.txt"
                lines = []
                for clip in clips_by_camera[camera]:
                    escaped = str(clip.resolve()).replace("'", "'\\''")
                    lines.append(f"file '{escaped}'")
                concat.write_text("\n".join(lines) + "\n", encoding="utf-8")
                command.extend(["-f", "concat", "-safe", "0", "-i", str(concat)])

            filters = []
            for index in range(4):
                width, height = ((960, 720) if index == 0 else (320, 240))
                filters.append(
                    f"[{index}:v]trim=start={start:.3f}:duration={duration:.3f},"
                    f"setpts=PTS-STARTPTS,scale={width}:{height}:force_original_aspect_ratio=decrease,"
                    f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black[v{index}]"
                )
            filters.append(
                "[v0][v1][v2][v3]xstack=inputs=4:"
                "layout=0_0|960_0|960_240|960_480:fill=black:shortest=1[outv]"
            )
            command.extend(
                [
                    "-filter_complex", ";".join(filters), "-map", "[outv]", "-an",
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "27",
                    "-pix_fmt", "yuv420p", "-r", "24", "-threads", "2",
                    "-movflags", "+faststart", str(partial_video),
                ]
            )
            runner(command, check=True)
            runner(
                [
                    "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-ss", "1",
                    "-i", str(partial_video), "-frames:v", "1", "-vf", "scale=640:-2",
                    "-q:v", "3", str(partial_preview),
                ],
                check=True,
            )
        if partial_video.stat().st_size == 0 or partial_preview.stat().st_size == 0:
            raise RuntimeError("FFmpeg produced an empty derivative")
        os.replace(partial_video, video)
        os.replace(partial_preview, preview)
    except Exception:
        partial_video.unlink(missing_ok=True)
        partial_preview.unlink(missing_ok=True)
        raise


def event_window(event: Path) -> tuple[float, float]:
    """Return the ten-seconds-before/twenty-after event window."""
    event_time = _event_time(event)
    groups = []
    for path in select_recent_clips(event):
        match = CLIP_PATTERN.match(path.name)
        if match:
            groups.append(datetime.strptime(match.group("group"), "%Y-%m-%d_%H-%M-%S"))
    if not groups:
        raise ValueError("event has no complete camera groups")
    start = max(0.0, (event_time - min(groups)).total_seconds() - 10.0)
    return start, 30.0


def focus_camera(event: Path) -> str:
    metadata = json.loads((event / "event.json").read_text(encoding="utf-8"))
    return {
        "3": "left_repeater",
        "5": "left_repeater",
        "4": "right_repeater",
        "6": "right_repeater",
        "7": "back",
    }.get(str(metadata.get("camera", "")), "front")


def object_keys(event_key: str, revision: str) -> dict[str, str]:
    category, separator, event_name = event_key.partition("/")
    if separator != "/" or category not in ALLOWED_CATEGORIES:
        raise ValueError("invalid event key")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", event_name) or event_name in {".", ".."}:
        raise ValueError("unsafe event name")
    if not re.fullmatch(r"[a-fA-F0-9]+", revision):
        raise ValueError("unsafe revision")
    base = f"{object_prefix()}/{category}/{event_name}/{revision}"
    return {
        "video": f"{base}/composite.mp4",
        "preview": f"{base}/preview.jpg",
        "metadata": f"{base}/event.json",
    }


def alert_marker_path(alerts: Path, event_key: str) -> Path:
    object_keys(event_key, "0")
    return alerts / f"{hashlib.sha256(event_key.encode('utf-8')).hexdigest()}.json"


def alert_was_sent(alerts: Path, event_key: str) -> bool:
    marker = alert_marker_path(alerts, event_key)
    try:
        return bool(json.loads(marker.read_text(encoding="utf-8")).get("sent"))
    except (FileNotFoundError, OSError, ValueError, json.JSONDecodeError):
        return False


class SourceChanged(RuntimeError):
    """Raised when the car changes an event while it is being copied."""


class LiveMount:
    """Provide a freshly mounted read-only view for exactly one scan.

    This protects against Pi-side writes and refreshes filesystem metadata, but
    it is not a transactional snapshot while the car writes through USB. The
    exclusive gadget-cycle lock serializes host-side SentryUSB maintenance;
    CaptureEngine separately requires matching signatures across fresh mounts
    and before/after copying before publishing an event.
    """

    def __init__(
        self,
        image: Path,
        mountpoint: Path,
        lock_path: Path,
        *,
        mountopts_helper: Path = Path("/root/bin/mountoptsforimage"),
        runner: Callable[..., object] = subprocess.run,
        unmount_retries: int = 3,
    ) -> None:
        self.image = image
        self.mountpoint = mountpoint
        self.lock_path = lock_path
        self.mountopts_helper = mountopts_helper
        self.runner = runner
        if unmount_retries < 1:
            raise ValueError("unmount_retries must be positive")
        self.unmount_retries = unmount_retries

    @contextmanager
    def mounted(self) -> Iterator[Path]:
        self.mountpoint.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        mounted = False
        with self.lock_path.open("a+b") as lock_handle:
            fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX)
            try:
                result = self.runner(
                    [str(self.mountopts_helper), str(self.image)],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                output = str(getattr(result, "stdout", "")).strip()
                fstype, options = output.split(maxsplit=1)
                safe_options = [
                    option for option in options.split(",") if not option.startswith("umask=")
                ]
                option_string = ",".join(
                    ["ro", "nodev", "nosuid", "noexec", "umask=0077", *safe_options]
                )
                self.runner(
                    [
                        "mount", "-t", fstype, "-o", option_string,
                        str(self.image), str(self.mountpoint),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                mounted = True
                yield self.mountpoint
            finally:
                if mounted:
                    for attempt in range(self.unmount_retries):
                        try:
                            self.runner(
                                ["umount", str(self.mountpoint)],
                                check=True,
                                capture_output=True,
                                text=True,
                            )
                            break
                        except subprocess.CalledProcessError:
                            if attempt + 1 < self.unmount_retries:
                                time.sleep(0.1)
                    else:
                        self.runner(
                            ["umount", "-l", str(self.mountpoint)],
                            check=True,
                            capture_output=True,
                            text=True,
                        )
                fcntl.flock(lock_handle.fileno(), fcntl.LOCK_UN)
        try:
            self.mountpoint.rmdir()
        except OSError:
            pass


class PushoverClient:
    ENDPOINT = "https://api.pushover.net/1/messages.json"

    def __init__(self, token: str, user: str, *, opener=urlopen) -> None:
        if not token or not user:
            raise ValueError("Pushover credentials are required")
        self._token = token
        self._user = user
        self._opener = opener

    def send(self, title: str, message: str, **kwargs: object) -> None:
        fields = {
            "token": self._token,
            "user": self._user,
            "title": title,
            "message": message,
        }
        url = kwargs.get("url")
        if url:
            fields["url"] = str(url)
            fields["url_title"] = "View combined video"
        request = Request(
            self.ENDPOINT,
            data=urlencode(fields).encode("utf-8"),
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                "User-Agent": "SentryUSB-Event-Processor/1",
            },
            method="POST",
        )
        with self._opener(request, timeout=20) as response:
            result = json.loads(response.read())
        if result.get("status") != 1:
            raise RuntimeError("Pushover rejected the notification")


class S3Uploader:
    def __init__(self, client: Any, bucket: str) -> None:
        self.client = client
        self.bucket = bucket

    @staticmethod
    def _validate_key(key: str) -> None:
        prefix = f"{object_prefix()}/"
        if not key.startswith(prefix):
            raise ValueError("S3 key is outside the allowed prefix")
        parts = key[len(prefix) :].split("/")
        if (
            len(parts) < 2
            or parts[0] not in ALLOWED_CATEGORIES
            or any(part in {"", ".", ".."} for part in parts)
            or any(not re.fullmatch(r"[A-Za-z0-9_.-]+", part) for part in parts)
        ):
            raise ValueError("S3 key is malformed")

    def upload(self, path: Path, key: str, content_type: str) -> None:
        self._validate_key(key)
        self.client.upload_file(
            str(path), self.bucket, key, ExtraArgs={"ContentType": content_type}
        )

    def presign(self, key: str, expires: int) -> str:
        self._validate_key(key)
        return str(
            self.client.generate_presigned_url(
                "get_object",
                Params={"Bucket": self.bucket, "Key": key},
                ExpiresIn=expires,
            )
        )


class Notifier(Protocol):
    def send(self, title: str, message: str, **kwargs: object) -> None: ...


class Uploader(Protocol):
    def upload(self, path: Path, key: str, content_type: str) -> None: ...

    def presign(self, key: str, expires: int) -> str: ...


class AlertWorker:
    """Deliver a durable pre-copy event notification exactly once in normal operation."""

    def __init__(self, notifier: Notifier, *, clock: Callable[[], float] = time.time) -> None:
        self.notifier = notifier
        self.clock = clock

    @staticmethod
    def _save(marker: Path, state: dict[str, object]) -> None:
        temporary = marker.with_name(f".{marker.name}.{uuid.uuid4().hex}.partial")
        try:
            temporary.write_text(json.dumps(state, sort_keys=True) + "\n", encoding="utf-8")
            os.chmod(temporary, 0o640)
            with temporary.open("rb") as handle:
                os.fsync(handle.fileno())
            os.replace(temporary, marker)
            parent_fd = os.open(marker.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent_fd)
            finally:
                os.close(parent_fd)
        finally:
            temporary.unlink(missing_ok=True)

    def process(self, marker: Path) -> bool:
        if marker.is_symlink() or not marker.is_file():
            raise ValueError("alert marker must be a regular file")
        state = json.loads(marker.read_text(encoding="utf-8"))
        event_key = str(state["event_key"])
        object_keys(event_key, "0")
        if bool(state.get("sent")):
            return False
        if float(str(state.get("retry_after", 0))) > self.clock():
            return False
        message = f"Sentry event at {state.get('timestamp', 'unknown time')}"
        try:
            self.notifier.send(f"{vehicle_label()} Sentry event detected", message)
        except Exception:
            attempts = int(str(state.get("failure_attempts", 0))) + 1
            state["failure_attempts"] = attempts
            state["retry_after"] = self.clock() + min(900, 10 * (2 ** min(attempts - 1, 6)))
            self._save(marker, state)
            raise
        state["sent"] = True
        state.pop("failure_attempts", None)
        state.pop("retry_after", None)
        self._save(marker, state)
        return True


class EventWorker:
    """Advance one queued event through notification, processing, and S3 states."""

    def __init__(
        self,
        notifier: Notifier,
        uploader: Uploader,
        *,
        composer=compose_event,
        clock: Callable[[], float] = time.time,
        on_error: Callable[[Path, str, Exception], None] | None = None,
        send_detected: bool = True,
    ) -> None:
        self.notifier = notifier
        self.uploader = uploader
        self.composer = composer
        self.clock = clock
        self.on_error = on_error or (lambda event, stage, error: None)
        self.send_detected = send_detected

    @staticmethod
    def _load_state(event: Path) -> dict[str, object]:
        path = event / "state.json"
        if not path.exists():
            return {
                "detected_sent": False,
                "processed": False,
                "uploaded": False,
                "ready_sent": False,
            }
        return json.loads(path.read_text(encoding="utf-8"))

    @staticmethod
    def _save_state(event: Path, state: dict[str, object]) -> None:
        path = event / "state.json"
        temporary = event / ".state.json.partial"
        temporary.write_text(json.dumps(state, sort_keys=True) + "\n", encoding="utf-8")
        os.chmod(temporary, 0o640)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())
        os.replace(temporary, path)

    def _record_failure(
        self, event: Path, state: dict[str, object], stage: str, error: Exception
    ) -> None:
        attempts = int(str(state.get("failure_attempts", 0))) + 1
        state["failure_attempts"] = attempts
        state["last_error_stage"] = stage
        state["retry_after"] = self.clock() + min(900, 10 * (2 ** min(attempts - 1, 6)))
        self._save_state(event, state)
        self.on_error(event, stage, error)

    @staticmethod
    def _clear_failure(state: dict[str, object]) -> None:
        for key in ("failure_attempts", "last_error_stage", "retry_after"):
            state.pop(key, None)

    def process(self, event: Path, *, detected_already_sent: bool = False) -> bool:
        capture = json.loads((event / "capture.json").read_text(encoding="utf-8"))
        metadata = json.loads((event / "event.json").read_text(encoding="utf-8"))
        state = self._load_state(event)
        if detected_already_sent and not state["detected_sent"]:
            state["detected_sent"] = True
            self._save_state(event, state)
        if float(str(state.get("retry_after", 0))) > self.clock():
            return False
        message = f"Sentry event at {metadata.get('timestamp', 'unknown time')}"

        if not state["detected_sent"] and self.send_detected:
            try:
                self.notifier.send(f"{vehicle_label()} Sentry event detected", message)
                state["detected_sent"] = True
                self._save_state(event, state)
            except Exception as error:
                self._record_failure(event, state, "detected-notification", error)

        derived = event / "derived"
        video = derived / "composite.mp4"
        preview = derived / "preview.jpg"
        if not state["processed"]:
            try:
                derived.mkdir(mode=0o750, exist_ok=True)
                self.composer(event, video, preview)
                state["processed"] = True
                self._save_state(event, state)
            except Exception as error:
                self._record_failure(event, state, "composition", error)
                return False

        keys = object_keys(str(capture["event_key"]), str(capture["signature"])[:12])
        if not state["uploaded"]:
            try:
                self.uploader.upload(video, keys["video"], "video/mp4")
                self.uploader.upload(preview, keys["preview"], "image/jpeg")
                self.uploader.upload(event / "event.json", keys["metadata"], "application/json")
                state["uploaded"] = True
                self._save_state(event, state)
            except Exception as error:
                self._record_failure(event, state, "upload", error)
                return False

        if state["uploaded"] and state["detected_sent"] and not state["ready_sent"]:
            try:
                url = self.uploader.presign(keys["video"], 7 * 24 * 60 * 60)
                self.notifier.send(f"{vehicle_label()} Sentry video ready", message, url=url)
                state["ready_sent"] = True
                self._clear_failure(state)
                self._save_state(event, state)
            except Exception as error:
                self._record_failure(event, state, "ready-notification", error)
                return False

        return bool(
            state["detected_sent"]
            and state["processed"]
            and state["uploaded"]
            and state["ready_sent"]
        )


class CaptureEngine:
    """Durably copy new live events into an atomic processing inbox."""

    def __init__(
        self,
        state_dir: Path,
        inbox: Path,
        *,
        alerts: Path | None = None,
        copy_file: Callable[[Path, Path], object] = shutil.copy2,
        owner_uid: int | None = None,
        owner_gid: int | None = None,
        instant_alerts_enabled: bool = True,
        video_enabled: bool = True,
    ) -> None:
        self.state_dir = state_dir
        self.inbox = inbox
        self.alerts = alerts or inbox.parent / "alerts"
        self.copy_file = copy_file
        self.owner_uid = owner_uid
        self.owner_gid = owner_gid
        self.instant_alerts_enabled = instant_alerts_enabled
        self.video_enabled = video_enabled
        state_dir.mkdir(parents=True, exist_ok=True)
        inbox.mkdir(parents=True, exist_ok=True)
        self.alerts.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(state_dir / "capture.sqlite")
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS events (event_key TEXT PRIMARY KEY, signature TEXT NOT NULL, disposition TEXT NOT NULL)"
        )
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)"
        )
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS candidates (event_key TEXT PRIMARY KEY, signature TEXT NOT NULL, observations INTEGER NOT NULL)"
        )
        self.db.commit()

    def close(self) -> None:
        self.db.close()

    def __enter__(self) -> "CaptureEngine":
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    @staticmethod
    def _key(event: Path) -> str:
        return f"{event.parent.name}/{event.name}"

    def bootstrap(self, events: list[Path]) -> int:
        if self.db.execute("SELECT 1 FROM metadata WHERE key='bootstrapped'").fetchone():
            return 0
        for event in events:
            self.db.execute(
                "INSERT OR REPLACE INTO events(event_key,signature,disposition) VALUES(?,?,?)",
                (self._key(event), event_signature(event), "ignored-existing"),
            )
        self.db.execute("DELETE FROM candidates")
        self.db.execute("INSERT INTO metadata(key,value) VALUES('bootstrapped','1')")
        self.db.commit()
        return len(events)

    def _queue_alert(self, event: Path, event_key: str) -> Path:
        marker = alert_marker_path(self.alerts, event_key)
        if marker.exists():
            return marker
        metadata = json.loads((event / "event.json").read_text(encoding="utf-8"))
        state = {
            "event_key": event_key,
            "timestamp": str(metadata.get("timestamp", "unknown time")),
            "sent": False,
        }
        temporary = marker.with_name(f".{marker.name}.{uuid.uuid4().hex}.partial")
        try:
            temporary.write_text(json.dumps(state, sort_keys=True) + "\n", encoding="utf-8")
            os.chmod(temporary, 0o640)
            if self.owner_uid is not None or self.owner_gid is not None:
                os.chown(
                    temporary,
                    -1 if self.owner_uid is None else self.owner_uid,
                    -1 if self.owner_gid is None else self.owner_gid,
                    follow_symlinks=False,
                )
            with temporary.open("rb") as handle:
                os.fsync(handle.fileno())
            os.replace(temporary, marker)
            parent_fd = os.open(self.alerts, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent_fd)
            finally:
                os.close(parent_fd)
        finally:
            temporary.unlink(missing_ok=True)
        return marker

    def _set_owner(self, root: Path) -> None:
        if self.owner_uid is None and self.owner_gid is None:
            return
        uid = -1 if self.owner_uid is None else self.owner_uid
        gid = -1 if self.owner_gid is None else self.owner_gid
        os.chown(root, uid, gid, follow_symlinks=False)
        for directory, directories, files in os.walk(root, followlinks=False):
            for name in [*directories, *files]:
                os.chown(Path(directory) / name, uid, gid, follow_symlinks=False)

    def _ingest(self, event: Path, signature: str) -> Path:
        event_id = f"{event.parent.name}--{event.name}--{signature[:12]}"
        destination = self.inbox / event_id
        if destination.exists():
            return destination
        temporary = self.inbox / f".{event_id}.partial-{uuid.uuid4().hex}"
        temporary.mkdir(mode=0o750)
        try:
            selected = [event / "event.json", *select_recent_clips(event)]
            for source in selected:
                self.copy_file(source, temporary / source.name)
            if event_signature(event) != signature:
                raise SourceChanged(f"source changed during capture: {self._key(event)}")
            manifest = {
                "event_key": self._key(event),
                "signature": signature,
                "captured_files": [path.name for path in selected],
            }
            manifest_path = temporary / "capture.json"
            manifest_path.write_text(json.dumps(manifest, sort_keys=True) + "\n", encoding="utf-8")
            for path in temporary.iterdir():
                with path.open("rb") as handle:
                    os.fsync(handle.fileno())
                os.chmod(path, 0o640)
            self._set_owner(temporary)
            os.replace(temporary, destination)
            parent_fd = os.open(self.inbox, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent_fd)
            finally:
                os.close(parent_fd)
            return destination
        except Exception:
            shutil.rmtree(temporary, ignore_errors=True)
            raise

    def capture(self, events: list[Path]) -> list[Path]:
        captured: list[Path] = []
        for event in events:
            signature = event_signature(event)
            event_key = self._key(event)
            row = self.db.execute(
                "SELECT signature FROM events WHERE event_key=?", (event_key,)
            ).fetchone()
            if row and row[0] == signature:
                self.db.execute("DELETE FROM candidates WHERE event_key=?", (event_key,))
                continue
            candidate = self.db.execute(
                "SELECT signature,observations FROM candidates WHERE event_key=?", (event_key,)
            ).fetchone()
            observations = candidate[1] + 1 if candidate and candidate[0] == signature else 1
            self.db.execute(
                "INSERT OR REPLACE INTO candidates(event_key,signature,observations) VALUES(?,?,?)",
                (event_key, signature, observations),
            )
            self.db.commit()
            if observations < 2:
                continue
            if self.instant_alerts_enabled:
                self._queue_alert(event, event_key)
            destination = self._ingest(event, signature) if self.video_enabled else None
            if self.video_enabled and self.instant_alerts_enabled:
                disposition = "captured-alert-and-video"
            elif self.video_enabled:
                disposition = "captured-video-only"
            elif self.instant_alerts_enabled:
                disposition = "captured-alert-only"
            else:
                disposition = "suppressed"
            self.db.execute(
                "INSERT OR REPLACE INTO events(event_key,signature,disposition) VALUES(?,?,?)",
                (event_key, signature, disposition),
            )
            self.db.execute("DELETE FROM candidates WHERE event_key=?", (event_key,))
            self.db.commit()
            if destination is not None:
                captured.append(destination)
        return captured

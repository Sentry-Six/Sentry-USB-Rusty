#!/usr/bin/env python3
"""Long-running capture and processing service entry points."""

from __future__ import annotations

import argparse
import json
import os
import pwd
import re
import stat
import sys
import time
from pathlib import Path
from urllib.parse import urlparse

from processor import (
    AlertWorker,
    CaptureEngine,
    EventWorker,
    LiveMount,
    PushoverClient,
    S3Uploader,
    alert_was_sent,
    discover_events,
)

BASE = Path("/backingfiles/teslabox-processor")


def log(message: str) -> None:
    print(message, flush=True)


def load_json_secret(path: Path) -> dict[str, str]:
    info = os.lstat(path)
    mode = stat.S_IMODE(info.st_mode)
    credentials_directory = os.environ.get("CREDENTIALS_DIRECTORY")
    systemd_projected = (
        mode == 0o440
        and credentials_directory is not None
        and path.parent.resolve() == Path(credentials_directory).resolve()
    )
    if not stat.S_ISREG(info.st_mode) or (mode & 0o077 and not systemd_projected):
        raise PermissionError(
            "credential must be mode 600, or mode 440 inside systemd's credential directory"
        )
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in data.items()):
        raise ValueError("credential must contain a JSON string map")
    return data


def read_feature_flags(
    environment: dict[str, str] | os._Environ[str] | None = None,
) -> tuple[bool, bool]:
    env = os.environ if environment is None else environment

    def flag(name: str) -> bool:
        value = str(env.get(name, "true")).strip().lower()
        if value not in {"true", "false"}:
            raise ValueError(f"{name} must be true or false")
        return value == "true"

    return (
        flag("SENTRYUSB_INSTANT_ALERTS_ENABLED"),
        flag("SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED"),
    )


def run_capture(args: argparse.Namespace) -> int:
    instant_alerts_enabled, video_enabled = read_feature_flags()
    account = pwd.getpwnam(args.worker_user)
    observer = LiveMount(
        Path(args.image),
        Path(args.mountpoint),
        Path(args.lock),
        mountopts_helper=Path(args.mountopts_helper),
    )
    with CaptureEngine(
        Path(args.state),
        Path(args.inbox),
        alerts=Path(args.alerts),
        owner_uid=account.pw_uid,
        owner_gid=account.pw_gid,
        instant_alerts_enabled=instant_alerts_enabled,
        video_enabled=video_enabled,
    ) as engine:
        while True:
            try:
                with observer.mounted() as mounted:
                    events = discover_events(mounted / "TeslaCam")
                    ignored = engine.bootstrap(events)
                    if ignored:
                        log(f"bootstrapped existing_events={ignored}")
                    for captured in engine.capture(events):
                        log(f"captured event={captured.name}")
            except Exception as error:
                errno_detail = f" errno={error.errno}" if isinstance(error, OSError) else ""
                log(f"capture retry error_type={type(error).__name__}{errno_detail}")
            time.sleep(args.interval)


def run_alert(args: argparse.Namespace) -> int:
    pushover = load_json_secret(Path(args.pushover_credential))
    if not pushover.get("token") or not pushover.get("user"):
        raise ValueError("Pushover credential is incomplete")
    worker = AlertWorker(PushoverClient(pushover["token"], pushover["user"]))
    alerts = Path(args.alerts)
    while True:
        for marker in sorted(alerts.glob("*.json") if alerts.is_dir() else []):
            try:
                if worker.process(marker):
                    log(f"alert sent marker={marker.stem}")
            except Exception as error:
                log(f"alert retry marker={marker.stem} error_type={type(error).__name__}")
        time.sleep(args.interval)


def resolve_s3_configuration(
    legacy: dict[str, str], environment: dict[str, str] | os._Environ[str] | None = None
) -> dict[str, str]:
    """Resolve SentryUSB-style exported variables before the legacy JSON fields."""
    env = os.environ if environment is None else environment

    def configured(name: str, legacy_name: str, default: str = "") -> str:
        return str(env.get(name) or legacy.get(legacy_name) or default).strip()

    endpoint = configured("S3_ENDPOINT_URL", "endpoint")
    bucket = configured("S3_BUCKET", "bucket")
    region = str(
        env.get("AWS_REGION")
        or env.get("AWS_DEFAULT_REGION")
        or legacy.get("region")
        or ""
    ).strip()
    access_key = configured("AWS_ACCESS_KEY_ID", "access_key")
    secret_key = configured("AWS_SECRET_ACCESS_KEY", "secret_key")
    session_token = configured("AWS_SESSION_TOKEN", "session_token")

    parsed = urlparse(endpoint)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or parsed.path not in {"", "/"}
    ):
        raise ValueError("S3_ENDPOINT_URL must be an HTTPS origin without credentials or a path")
    endpoint = endpoint.rstrip("/")
    if not re.fullmatch(r"(?=.{3,63}$)[a-z0-9][a-z0-9.-]*[a-z0-9]", bucket):
        raise ValueError("S3 bucket name is invalid")
    if not region or not re.fullmatch(r"[A-Za-z0-9._-]+", region):
        raise ValueError("AWS region is invalid")
    if not access_key or not secret_key:
        raise ValueError("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY are required")

    result = {
        "endpoint": endpoint,
        "bucket": bucket,
        "region": region,
        "access_key": access_key,
        "secret_key": secret_key,
    }
    if session_token:
        result["session_token"] = session_token
    return result


def build_uploader(
    secret: dict[str, str], environment: dict[str, str] | os._Environ[str] | None = None
) -> S3Uploader:
    configuration = resolve_s3_configuration(secret, environment)
    import boto3
    from botocore.config import Config

    client = boto3.client(
        "s3",
        endpoint_url=configuration["endpoint"],
        aws_access_key_id=configuration["access_key"],
        aws_secret_access_key=configuration["secret_key"],
        aws_session_token=configuration.get("session_token"),
        region_name=configuration["region"],
        config=Config(signature_version="s3v4", s3={"addressing_style": "path"}),
        verify=True,
    )
    return S3Uploader(client, bucket=configuration["bucket"])


def run_worker(args: argparse.Namespace) -> int:
    instant_alerts_enabled, video_enabled = read_feature_flags()
    if not video_enabled:
        raise ValueError("video worker started while SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED=false")
    pushover = load_json_secret(Path(args.pushover_credential))
    if not pushover.get("token") or not pushover.get("user"):
        raise ValueError("Pushover credential is incomplete")
    notifier = PushoverClient(pushover["token"], pushover["user"])
    uploader = build_uploader(load_json_secret(Path(args.s3_credential)))
    worker = EventWorker(
        notifier,
        uploader,
        on_error=lambda event, stage, error: log(
            f"worker retry event={event.name} stage={stage} error_type={type(error).__name__}"
        ),
        send_detected=False,
    )
    inbox = Path(args.inbox)
    alerts = Path(args.alerts)
    reported_complete: set[str] = set()
    while True:
        for event in sorted(inbox.iterdir() if inbox.is_dir() else []):
            if not event.is_dir() or event.name.startswith("."):
                continue
            try:
                capture = json.loads((event / "capture.json").read_text(encoding="utf-8"))
                detected_sent = (
                    not instant_alerts_enabled
                    or alert_was_sent(alerts, str(capture["event_key"]))
                )
                complete = worker.process(event, detected_already_sent=detected_sent)
                if complete and event.name not in reported_complete:
                    log(f"complete event={event.name}")
                    reported_complete.add(event.name)
                elif not complete:
                    reported_complete.discard(event.name)
            except Exception as error:
                log(f"worker retry event={event.name} error_type={type(error).__name__}")
        time.sleep(args.interval)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)

    capture = commands.add_parser("capture")
    capture.add_argument("--image", default="/backingfiles/cam_disk.bin")
    capture.add_argument("--mountpoint", default="/run/teslabox-live")
    capture.add_argument("--lock", default="/tmp/sentryusb_gadget_cycle.lock")
    capture.add_argument("--mountopts-helper", default="/root/bin/mountoptsforimage")
    capture.add_argument("--state", default=str(BASE / "capture-state"))
    capture.add_argument("--inbox", default=str(BASE / "inbox"))
    capture.add_argument("--alerts", default=str(BASE / "alerts"))
    capture.add_argument("--worker-user", default="teslabox-processor")
    capture.add_argument("--interval", type=float, default=3.0)
    capture.set_defaults(function=run_capture)

    alert = commands.add_parser("alert")
    alert.add_argument("--alerts", default=str(BASE / "alerts"))
    alert.add_argument("--pushover-credential", required=True)
    alert.add_argument("--interval", type=float, default=1.0)
    alert.set_defaults(function=run_alert)

    worker = commands.add_parser("worker")
    worker.add_argument("--inbox", default=str(BASE / "inbox"))
    worker.add_argument("--alerts", default=str(BASE / "alerts"))
    worker.add_argument("--pushover-credential", required=True)
    worker.add_argument("--s3-credential", required=True)
    worker.add_argument("--interval", type=float, default=10.0)
    worker.set_defaults(function=run_worker)
    return result


def main() -> int:
    args = parser().parse_args()
    return int(args.function(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(0)

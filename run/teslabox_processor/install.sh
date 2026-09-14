#!/bin/bash
set -euo pipefail

EXPECTED_HOSTNAME=${EXPECTED_HOSTNAME:-}
EXPECTED_MACHINE_ID_SHA256=${EXPECTED_MACHINE_ID_SHA256:-}
SENTRYUSB_VEHICLE_LABEL=${SENTRYUSB_VEHICLE_LABEL:-${TESLABOX_VEHICLE_LABEL:-Tesla}}
BASE=/backingfiles/teslabox-processor
S3_DEST=$BASE/secrets/s3.json
PACKAGE_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
S3_STAGING=${S3_STAGING:-/tmp/sentryusb-event-s3.json}
ROOT_WAS_RO=false
SENSITIVE_TEMP=
TRANSACTION_STARTED=false
COMMITTED=false
BACKUP=
UNITS=(teslabox-capture.service teslabox-alert.service teslabox-processor.service)
cleanup_sensitive() {
  if [ -n "$SENSITIVE_TEMP" ]; then
    rm -f -- "$SENSITIVE_TEMP"
  fi
}
trap cleanup_sensitive EXIT HUP INT TERM

if [ -n "$EXPECTED_HOSTNAME" ] && [ "$(hostname -s)" != "$EXPECTED_HOSTNAME" ]; then
  echo "refusing unexpected hostname" >&2
  exit 2
fi
if [ -n "$EXPECTED_MACHINE_ID_SHA256" ] && [ "$(sha256sum /etc/machine-id | cut -d' ' -f1)" != "$EXPECTED_MACHINE_ID_SHA256" ]; then
  echo "refusing unexpected machine identity" >&2
  exit 2
fi
cd "$PACKAGE_DIR"
sha256sum --check --strict MANIFEST.sha256
python3 -m venv --help >/dev/null

# SentryUSB's supported configuration contract is exported uppercase variables
# loaded by envsetup.sh from /root/sentryusb.conf.
set +u
source /root/bin/envsetup.sh >/dev/null
set -u
S3_PREFIX=${S3_PREFIX:-${TESLABOX_OBJECT_PREFIX:-}}
SENTRYUSB_INSTANT_ALERTS_ENABLED=${SENTRYUSB_INSTANT_ALERTS_ENABLED:-true}
SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED=${SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED:-true}
for setting in SENTRYUSB_INSTANT_ALERTS_ENABLED SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED; do
  value=${!setting,,}
  if [ "$value" != true ] && [ "$value" != false ]; then
    echo "$setting must be true or false" >&2
    exit 3
  fi
  printf -v "$setting" %s "$value"
done
if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ] && [ -z "$S3_PREFIX" ]; then
  echo "S3_PREFIX is required when video notifications are enabled" >&2
  exit 3
fi
if { [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ] || [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; } && [ -z "$SENTRYUSB_VEHICLE_LABEL" ]; then
  echo "SENTRYUSB_VEHICLE_LABEL must not be empty" >&2
  exit 3
fi

if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  command -v ffmpeg >/dev/null
  command -v ffprobe >/dev/null
fi
if { [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ] || [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; } && \
   { [ -z "${PUSHOVER_APP_KEY:-}" ] || [ -z "${PUSHOVER_USER_KEY:-}" ]; }; then
  echo "Pushover is required when instant alerts or video notifications are enabled" >&2
  exit 3
fi

S3_SOURCE=
GENERATED_S3=false
if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  if [ -n "${AWS_ACCESS_KEY_ID:-}" ] || [ -n "${AWS_SECRET_ACCESS_KEY:-}" ]; then
    for setting in AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_REGION S3_ENDPOINT_URL S3_BUCKET; do
      if [ -z "${!setting:-}" ]; then
        echo "$setting is required when standard S3 variables are used" >&2
        exit 3
      fi
    done
    S3_SOURCE=$(mktemp /tmp/sentryusb-event-standard-s3.XXXXXX)
    SENSITIVE_TEMP=$S3_SOURCE
    GENERATED_S3=true
    umask 077
    export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_REGION S3_ENDPOINT_URL S3_BUCKET S3_PREFIX
    export AWS_SESSION_TOKEN=${AWS_SESSION_TOKEN:-}
    python3 - "$S3_SOURCE" <<'PY'
import json, os, tempfile, sys
path = sys.argv[1]
data = {
    "access_key": os.environ["AWS_ACCESS_KEY_ID"],
    "secret_key": os.environ["AWS_SECRET_ACCESS_KEY"],
    "region": os.environ["AWS_REGION"],
    "endpoint": os.environ["S3_ENDPOINT_URL"],
    "bucket": os.environ["S3_BUCKET"],
    "prefix": os.environ["S3_PREFIX"],
}
if os.environ.get("AWS_SESSION_TOKEN"):
    data["session_token"] = os.environ["AWS_SESSION_TOKEN"]
fd, temporary = tempfile.mkstemp(prefix=".standard-s3.", dir="/tmp")
try:
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        json.dump(data, output, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)
finally:
    try: os.unlink(temporary)
    except FileNotFoundError: pass
PY
  elif [ -s "$S3_DEST" ]; then
    S3_SOURCE=$S3_DEST
  elif [ -s "$S3_STAGING" ]; then
    S3_SOURCE=$S3_STAGING
  else
    echo "S3 configuration is required when video notifications are enabled" >&2
    exit 3
  fi

  python3 - "$S3_SOURCE" "$S3_PREFIX" "$SENTRYUSB_VEHICLE_LABEL" <<'PY'
import json, os, re, stat, sys
from urllib.parse import urlparse
path, prefix, label = sys.argv[1:]
info = os.lstat(path)
if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077:
    raise SystemExit("S3 credential file must be a mode-600 regular file")
data = json.load(open(path, encoding="utf-8"))
endpoint = str(data.get("endpoint", "")).strip()
parsed = urlparse(endpoint)
if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in {"", "/"}:
    raise SystemExit("S3_ENDPOINT_URL must be an HTTPS origin without credentials or a path")
bucket = str(data.get("bucket", "")).strip()
if not re.fullmatch(r"(?=.{3,63}$)[a-z0-9][a-z0-9.-]*[a-z0-9]", bucket):
    raise SystemExit("invalid S3 bucket")
region = str(data.get("region", "")).strip()
if not region or not re.fullmatch(r"[A-Za-z0-9._-]+", region):
    raise SystemExit("invalid AWS region")
if data.get("prefix", prefix) != prefix:
    raise SystemExit("S3 credential prefix does not match S3_PREFIX")
if not data.get("access_key") or not data.get("secret_key"):
    raise SystemExit("incomplete S3 credential")
for name, value in (("prefix", prefix), ("label", label)):
    if not value or len(value) > 80 or any(ord(character) < 32 for character in value):
        raise SystemExit(f"invalid {name}")
if "/" in prefix or "\\" in prefix or prefix in {".", ".."}:
    raise SystemExit("invalid prefix")
PY
fi

if findmnt -n -o OPTIONS / | tr ',' '\n' | grep -qx ro; then
  ROOT_WAS_RO=true
fi
restore_root() {
  if [ "$ROOT_WAS_RO" = true ]; then
    mount -o remount,ro / || true
  fi
}
rollback_install() {
  if [ "$TRANSACTION_STARTED" != true ] || [ "$COMMITTED" = true ] || [ -z "$BACKUP" ]; then
    return
  fi
  set +e
  systemctl stop "${UNITS[@]}"
  rm -rf "$BASE/app" "$BASE/secrets"
  rm -f "$BASE/processor.env"
  for name in app secrets processor.env; do
    if [ -e "$BACKUP/$name" ]; then
      cp -a "$BACKUP/$name" "$BASE/$name"
    fi
  done
  mount -o remount,rw /
  for unit in "${UNITS[@]}"; do
    if [ -f "$BACKUP/units/$unit" ]; then
      install -o root -g root -m 0644 "$BACKUP/units/$unit" "/etc/systemd/system/$unit"
    else
      rm -f "/etc/systemd/system/$unit"
    fi
  done
  systemctl daemon-reload
  for unit in "${UNITS[@]}"; do
    if [ -e "$BACKUP/enabled/$unit" ]; then
      systemctl enable "$unit"
    else
      systemctl disable "$unit"
    fi
  done
  restore_root
  for unit in "${UNITS[@]}"; do
    if [ -e "$BACKUP/active/$unit" ]; then
      systemctl start "$unit"
    fi
  done
}
cleanup_all() {
  status=$?
  trap - EXIT HUP INT TERM
  cleanup_sensitive
  if [ "$TRANSACTION_STARTED" = true ] && [ "$COMMITTED" != true ]; then
    rollback_install
  fi
  restore_root
  exit "$status"
}
trap cleanup_all EXIT HUP INT TERM

install -d -m 0751 "$BASE"
install -d -m 0700 "$BASE/secrets" "$BASE/capture-state"
if ! id teslabox-processor >/dev/null 2>&1; then
  mount -o remount,rw /
  useradd --system --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin teslabox-processor
  restore_root
fi
WORKER_UID=$(id -u teslabox-processor)
WORKER_GID=$(id -g teslabox-processor)
install -d -o "$WORKER_UID" -g "$WORKER_GID" -m 0770 "$BASE/inbox" "$BASE/alerts"

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
BACKUP=$BASE/backups/$STAMP
install -d -m 0700 "$BACKUP" "$BACKUP/units" "$BACKUP/active" "$BACKUP/enabled"
for name in app secrets processor.env; do
  if [ -e "$BASE/$name" ]; then
    cp -a "$BASE/$name" "$BACKUP/$name"
  fi
done
for unit in "${UNITS[@]}"; do
  if [ -f "/etc/systemd/system/$unit" ]; then
    cp -a "/etc/systemd/system/$unit" "$BACKUP/units/$unit"
  fi
  if systemctl --quiet is-active "$unit"; then
    : > "$BACKUP/active/$unit"
  fi
  if systemctl --quiet is-enabled "$unit"; then
    : > "$BACKUP/enabled/$unit"
  fi
done
TRANSACTION_STARTED=true
systemctl stop "${UNITS[@]}" 2>/dev/null || true

install -d -m 0755 "$BASE/app"
install -o root -g root -m 0644 app/processor.py app/service.py app/requirements.lock "$BASE/app/"
python3 - "$BASE/processor.env" "$S3_PREFIX" "$SENTRYUSB_VEHICLE_LABEL" "$SENTRYUSB_INSTANT_ALERTS_ENABLED" "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" "$S3_SOURCE" <<'PY'
import json, os, sys, tempfile
path, prefix, label, instant, video, s3_source = sys.argv[1:]
def quote(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
settings = {
    "S3_PREFIX": prefix,
    "SENTRYUSB_VEHICLE_LABEL": label,
    "SENTRYUSB_INSTANT_ALERTS_ENABLED": instant,
    "SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED": video,
}
if video == "true":
    s3 = json.load(open(s3_source, encoding="utf-8"))
    settings.update({
        "S3_ENDPOINT_URL": str(s3["endpoint"]).rstrip("/"),
        "S3_BUCKET": str(s3["bucket"]),
        "AWS_REGION": str(s3["region"]),
    })
fd, temporary = tempfile.mkstemp(prefix=".processor-env.", dir=os.path.dirname(path))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        for name, value in settings.items():
            output.write(name + "=" + quote(value) + "\n")
        output.flush()
        os.fsync(output.fileno())
    os.chmod(temporary, 0o644)
    os.replace(temporary, path)
finally:
    try: os.unlink(temporary)
    except FileNotFoundError: pass
PY

if [ ! -x "$BASE/venv/bin/python" ]; then
  python3 -m venv "$BASE/venv"
fi
"$BASE/venv/bin/python" -m pip install --disable-pip-version-check --no-index --find-links "$PACKAGE_DIR/wheels" --require-hashes -r app/requirements.lock
if ! "$BASE/venv/bin/python" -c 'import boto3, botocore.config, botocore.exceptions' >/dev/null 2>&1; then
  "$BASE/venv/bin/python" -m pip install --disable-pip-version-check --no-index --find-links "$PACKAGE_DIR/wheels" --require-hashes --force-reinstall -r app/requirements.lock
fi
"$BASE/venv/bin/python" -c 'import boto3, botocore.config, botocore.exceptions'

umask 077
if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ] || [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  export PUSHOVER_APP_KEY PUSHOVER_USER_KEY
  python3 - "$BASE/secrets/pushover.json" <<'PY'
import json, os, sys, tempfile
path = sys.argv[1]
data = {"token": os.environ["PUSHOVER_APP_KEY"], "user": os.environ["PUSHOVER_USER_KEY"]}
fd, temporary = tempfile.mkstemp(prefix=".pushover.", dir=os.path.dirname(path))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        json.dump(data, output, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.chmod(temporary, 0o600)
    os.replace(temporary, path)
finally:
    try: os.unlink(temporary)
    except FileNotFoundError: pass
PY
else
  rm -f "$BASE/secrets/pushover.json"
fi
if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ] && [ "$S3_SOURCE" != "$S3_DEST" ]; then
  install -o root -g root -m 0600 "$S3_SOURCE" "$S3_DEST"
elif [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = false ]; then
  rm -f "$S3_DEST"
fi
if [ "$GENERATED_S3" = true ] || [ "$S3_SOURCE" = "$S3_STAGING" ]; then
  rm -f "$S3_SOURCE"
  SENSITIVE_TEMP=
fi
unset PUSHOVER_APP_KEY PUSHOVER_USER_KEY AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN

mount -o remount,rw /
if [ -f /etc/systemd/system/teslabox-capture.service ]; then
  install -d -m 0700 "$BASE/backups/$STAMP/units"
  cp -a /etc/systemd/system/teslabox-capture.service /etc/systemd/system/teslabox-alert.service /etc/systemd/system/teslabox-processor.service "$BASE/backups/$STAMP/units/" 2>/dev/null || true
fi
install -o root -g root -m 0644 units/teslabox-capture.service units/teslabox-alert.service units/teslabox-processor.service /etc/systemd/system/
systemctl daemon-reload
if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ] || [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  systemctl enable teslabox-capture.service
else
  systemctl disable --now teslabox-capture.service
fi
if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ]; then
  systemctl enable teslabox-alert.service
else
  systemctl disable --now teslabox-alert.service
fi
if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  systemctl enable teslabox-processor.service
else
  systemctl disable --now teslabox-processor.service
fi
restore_root

systemd-analyze verify /etc/systemd/system/teslabox-capture.service /etc/systemd/system/teslabox-alert.service /etc/systemd/system/teslabox-processor.service
if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ] || [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  systemctl restart teslabox-capture.service
  systemctl --quiet is-active teslabox-capture.service
fi
if [ "$SENTRYUSB_INSTANT_ALERTS_ENABLED" = true ]; then
  systemctl restart teslabox-alert.service
  systemctl --quiet is-active teslabox-alert.service
fi
if [ "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED" = true ]; then
  systemctl restart teslabox-processor.service
  systemctl --quiet is-active teslabox-processor.service
fi
findmnt -n -o OPTIONS / | tr ',' '\n' | grep -qx ro
COMMITTED=true
ROOT_WAS_RO=false
cleanup_sensitive
trap - EXIT HUP INT TERM
printf 'installed app=%s instant_alerts=%s video_notifications=%s root=ro\n' "$BASE/app" "$SENTRYUSB_INSTANT_ALERTS_ENABLED" "$SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED"

# SentryUSB Event Processor add-on

An optional companion for [SentryUSB](https://github.com/Sentry-Six/Sentry-USB-Rusty) that can:

- send an immediate Pushover notification when a complete SavedClips or SentryClips event appears;
- render a camera mosaic with `ffmpeg`, upload it and its metadata to an S3-compatible service, and send a presigned ready link;
- run alongside the official SentryUSB core without replacing or modifying the gadget controller.

The add-on never processes `RecentClips` or `TeslaTrackMode`. Runtime data is kept under `/backingfiles`, and the installer restores a read-only root filesystem after systemd changes.

> **Status:** experimental add-on. Test on a spare image and keep a recoverable backup before installing.

## Configuration

Add exported variables to `/root/sentryusb.conf`, using SentryUSB's existing configuration mechanism:

```bash
export SENTRYUSB_VEHICLE_LABEL='Family Tesla'

export PUSHOVER_APP_KEY='application-token'
export PUSHOVER_USER_KEY='user-key'

export AWS_ACCESS_KEY_ID='vehicle-scoped-access-key'
export AWS_SECRET_ACCESS_KEY='vehicle-scoped-secret-key'
# Optional for temporary AWS credentials:
# export AWS_SESSION_TOKEN='session-token'
export AWS_REGION='us-east-1'
export S3_ENDPOINT_URL='https://s3.example.net'
export S3_BUCKET='sentry-events'
export S3_PREFIX='vehicle-name'

export SENTRYUSB_INSTANT_ALERTS_ENABLED=true
export SENTRYUSB_VIDEO_NOTIFICATIONS_ENABLED=true
```

`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, and `AWS_REGION` use the AWS names already supported by SentryUSB. `S3_ENDPOINT_URL`, `S3_BUCKET`, and `S3_PREFIX` supply destination fields not currently defined by upstream.

`S3_PREFIX` is required whenever video notifications are enabled and must be one safe path component. Use credentials scoped to that bucket/prefix. The endpoint must be an HTTPS origin without embedded credentials, query, fragment, or path.

Optional installer identity guards can be supplied at invocation time:

```bash
EXPECTED_HOSTNAME=sentryusb \
EXPECTED_MACHINE_ID_SHA256="$(sha256sum /etc/machine-id | cut -d' ' -f1)" \
./install.sh
```

They are unset by default so the public package contains no appliance identity.

## Feature switches

Both switches accept only `true` or `false` and default to `true`:

| Instant alerts | Video notifications | Behavior |
|---|---|---|
| `true` | `true` | Immediate alert, event copy, video render/upload, and ready link. |
| `true` | `false` | Immediate alert only; no MP4 copy, ffmpeg, S3, or ready link. |
| `false` | `true` | Video render/upload and ready link without the immediate alert. |
| `false` | `false` | Record a suppressed event without copying MP4s or requiring Pushover/S3. |

After changing configuration, rerun the installer. It enables only the required services and removes credential copies that disabled features no longer need.

## Build an offline package

The target appliance may have no writable root or Internet access. Build on another Linux system with Python, then transfer the resulting archive:

```bash
python3 -m pip download \
  --only-binary=:all: \
  --platform manylinux2014_aarch64 \
  --python-version 311 \
  --implementation cp \
  --abi cp311 \
  --dest /tmp/sentryusb-wheels \
  --require-hashes \
  -r requirements.lock

python3 build_package.py \
  --wheelhouse /tmp/sentryusb-wheels \
  --output /tmp/sentryusb-event-processor.tar.gz
sha256sum /tmp/sentryusb-event-processor.tar.gz
```

Build using the Python ABI and architecture used by your SentryUSB image. The package manifest covers every payload file and the installer uses hash-locked, offline dependency installation.

## Install

1. Back up `/backingfiles` and verify that the official SentryUSB core is healthy.
2. Copy the package to the appliance and independently verify its SHA-256.
3. Extract it on a writable persistent filesystem or under `/tmp`.
4. Run the installer as root from the extracted package directory.

```bash
tar -xzf sentryusb-event-processor.tar.gz
cd teslabox-processor-package
sudo ./install.sh
```

The package directory retains the historical `teslabox-processor-package` name for compatibility with existing installations. Installed service/data names are likewise retained so upgrades do not create parallel workers or lose durable state.

The installer snapshots the previous app, environment, credentials, units, and service state under `/backingfiles/teslabox-processor/backups/`. Failed activation triggers rollback.

## Verify

```bash
systemctl is-active teslabox-capture.service
systemctl is-active teslabox-alert.service
systemctl is-active teslabox-processor.service
findmnt -n -o OPTIONS /
journalctl -u teslabox-capture.service \
  -u teslabox-alert.service \
  -u teslabox-processor.service --no-pager
```

Disabled feature units are expected to report inactive/disabled. The root mount should include `ro` after installation.

## Compatibility aliases

Existing installations may continue to use `TESLABOX_VEHICLE_LABEL`, `TESLABOX_OBJECT_PREFIX`, or a mode-`0600` staged `s3.json`. New installations should use `SENTRYUSB_VEHICLE_LABEL`, `S3_PREFIX`, and the standard exported AWS/S3 variables above.

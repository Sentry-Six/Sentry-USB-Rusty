# Building SentryUSB (Rust)

## Prerequisites

- Rust stable (1.89+; some locked dependencies need it)
- Node `^20.19.0 || >=22.12.0` and npm
- `cross` for cross-compilation: `cargo install cross`
- Docker (for `cross` and image builds)

## Web UI

The web UI is React + Vite. Source lives in `web/`; the build output is embedded
into the `sentryusb` binary at compile time via `rust_embed`.

```
cd web
npm ci --no-audit --no-fund
npm run build
cd ..
rm -rf crates/sentryusb/static
cp -r web/dist crates/sentryusb/static
```

## Rust binaries

Three binaries ship with the project:
- `sentryusb` — main daemon (HTTP + WebSocket + setup orchestrator)
- `sentryusb-tesla-telemetry` — BLE telemetry sampler (lazy-started after pairing)
- `sentryusb-ble-action` — Tesla BLE action CLI (from the `sentryusb-tesla-ble` package)

### Cross-compile for the Pi

64-bit (Pi 3A+/4/5/Zero 2):
```
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb-tesla-telemetry
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb-tesla-ble --bin sentryusb-ble-action
```

32-bit (armhf — Pi 3A+ with 32-bit Pi OS):
```
cross build --release --target armv7-unknown-linux-gnueabihf -p sentryusb
```
(Build the other two packages the same way with the armv7 target.)

Binaries land in `target/<target>/release/`.

### Native (Linux dev box)

```
cargo build --release
```

## SD card image

`image/build-image.sh` takes the newest official Raspberry Pi OS Lite
(64-bit) image, installs one Sentry USB release into it by running
`install-pi.sh` in image mode inside a chroot, and writes the image, its
checksum and a Raspberry Pi Imager manifest. It needs Linux, root, and an
arm64 host (or qemu-user binfmt for aarch64):

```
sudo image/build-image.sh --tag v3.29.0 --out deploy
```

The image keeps Pi OS's first boot as it ships, so the Wi-Fi, user,
hostname and SSH settings chosen in Imager apply on the user's card. Imager
2 only offers those settings for images opened through a manifest that
declares how the image is configured (`init_format`), which is why users
open the `.rpi-imager-manifest` rather than the `.img.xz` itself;
`image/imager.py` copies `init_format` and the board list from Raspberry
Pi's own entry for the base image.

## Deploy to an existing Pi

```
# Copy binary to the Pi and run install-pi.sh with its local path
scp target/aarch64-unknown-linux-gnu/release/sentryusb pi@<ip>:/tmp/
ssh pi@<ip> sudo -i
bash install-pi.sh /tmp/sentryusb
```

## Testing

- Rust unit tests: `cargo test --workspace --locked` (CI runs them on Linux; a few tests need Linux-only `/proc` and shell tools)
- Clippy: `cargo +1.99.0 clippy --workspace --all-targets --locked -- -D warnings` (CI pins Rust 1.99.0)
- Shell and Python harnesses: `test/*.test.sh`, `test/*.test.py`
- Web: `cd web && npm test && npm run lint`
- Lint: `bash check.sh` (runs ShellCheck against selected runtime scripts)

## Releasing

GitHub Releases are expected to host these artifacts (naming consumed by
`install-pi.sh`):

- `sentryusb-linux-arm64-a53` / `-a72` / `-a76` (per-CPU aarch64 variants)
- `sentryusb-linux-arm64` (backward-compat alias = a72 build)
- `sentryusb-linux-armv7`
- `sentryusb-tesla-telemetry-linux-*` and `sentryusb-ble-action-linux-*` (one
  per CPU variant, plus the `-linux-arm64` alias)

The `Build & Release` workflow (`.github/workflows/build.yml`) builds these on
native arm64 runners (armv7 is cross-compiled with GCC) and attaches them to
the release.

Once a stable release is out, the `SD Card Image` workflow
(`.github/workflows/image.yml`) builds the image for it and attaches three
more files and a first line in the release notes pointing to the manifest.
Publishing the release doesn't wait for it.

- `Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest`: what users
  download and double-click. Its name puts it at the top of the release
  page, which lists files by name.
- `sentryusb-raspberry-pi-image.img.xz` and its `.sha256`: the image the
  manifest points to. Its name sorts it in among the binaries, so people
  don't flash it without Imager's Wi-Fi and user settings.
- `sentryusb-imager-os-list.json`: the same entry in the format Raspberry
  Pi's own Imager list loads from us, at
  `releases/latest/download/sentryusb-imager-os-list.json`.

Automatic builds run only while the repository variable `SENTRYUSB_SD_IMAGE`
is `auto`. Pre-releases are skipped, but promoting one to a full release
builds its image. To rebuild or test, run the workflow by hand with a tag;
turn off "publish" to keep the result as a workflow artifact instead of
attaching it.

armv6 (Pi Zero W / Pi 1) is no longer built — the board is too underpowered
to run the daemon comfortably, and dropping the matrix entry keeps the
release artifact count manageable.

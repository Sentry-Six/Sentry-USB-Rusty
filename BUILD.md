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
sudo image/build-image.sh --tag <tag> --out deploy   # a release tag, e.g. v3.28.9
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

Release builds cover aarch64 and armv7. ARMv6 boards (Pi Zero W v1, Pi 1)
are outside the build matrix: they are too underpowered to run the daemon
comfortably, and leaving them out keeps the release artifact count manageable.

## Behavior notes for developers

Implementation details that sit behind the user-facing wiki pages.

### Storage repair tool selection

Repair Storage selects its executable from the kernel architecture and the
executable's ELF header, not the filesystem or the web server's architecture.
A Pi running a 64-bit ARM kernel with 32-bit userspace uses a private ARM64 XFS
tool and libraries. The read-only check, normal repair, log-replay retry, and
confirmed force repair all use the same selected tool, recorded in the repair log.

Setup, the installer, and the first startup migration after an update prepare a
checksum-verified tool cache on the root card for offline recovery. Existing
installations get it without rerunning setup or toggling Auto Repair.
Compatible native tools and already-verified caches are reused without downloading.
Update preparation runs in the background and only prepares the tool; it never
starts a repair. A failed download is logged and the update continues. Setup or
an actual repair can retry; offline recovery is ready once the tool is
available. If repair must prepare it while the root card is read-only, the temporary
cache lives in `/run` and disappears on reboot. If repair preparation fails, it
stops before storage is disconnected; it never falls back to a 32-bit tool.

If a writable cache is damaged, preparation moves it to a `.bad-*` directory
for inspection and rebuilds from checksum-verified packages. Writable preparation
keeps only the newest quarantined copy and removes older copies under the lock.
If cleanup fails, it logs a warning and keeps using an otherwise valid tool.
Interrupted downloads can be retried; abandoned `.prepare-*` trees are removed during the
next locked preparation. Cache reuse checks the full file inventory, hashes,
and that library symlinks stay inside the private tree. This detects accidental
damage; someone with root access could still rewrite both the files and their
local verification record.

Only one manual or boot-time repair can run in the server at a time. A second
request returns an already-in-progress message. When automatic repair cannot
prepare its tool, it retries on later boots; the matching push notification
is limited to once per 24 hours while `/mutable` can retain the notice marker.
A successful repair preflight or a healthy boot clears that notice limit.

The selector stops before repair on the uncommon reported architecture `armv8l`
rather than assuming ARM64 execution support. The normal `armv6l`/`armv7l`
paths use native 32-bit tool selection.

Force repair requires its destructive confirmation or the automatic
force-repair preference. Selecting a compatible executable does not make force
repair lossless, and a repair-tool failure alone does not prove SSD hardware failure.
Legacy setup attempts a normal mount for journal replay and stops with a
Repair Storage message if mounting fails; journal clearing is left to Repair Storage.

### Notification messages and history

All providers, including generic webhooks and Sentry Connect, receive the same
short event summary. Notification history holds the full message, exact
timestamp and provider error details; archive logs hold the operational
diagnostics. Credentials are removed from provider errors, and very large
provider responses may be truncated with an explicit marker. Older app versions
can read the full message; expanding details needs an app version with the
current history view. Pi and app can be upgraded independently.

The archive count refers to the current batch of pending files; earlier
progress checkpoints are not subtracted again. Drive mapping summaries separate
drives mapped earlier from those mapped in the current pass. An unavailable
archive does not necessarily mean the car was away from home.

Webhook and IFTTT payloads carry structured fields; the human-readable message
text is a short summary that can change between versions. Custom local
start/finish notification commands receive the same arguments and environment
as before. A provider accepting a notification does not establish that a device
received it or a person read it.

### Automatic stable updates

The **Install stable updates automatically** setting (Settings → Device →
Update Preferences) is off by default. When enabled, installation is considered
only after an archive cycle transfers new footage and all configured follow-up
work finishes successfully. Zero-file, cancelled, interrupted, failed and Travel
Mode cycles skip installation. Failed attempts can retry after the next
qualifying archive; there is no hourly cooldown.

After downloading, the archive must still be reachable. If Tesla BLE is paired,
the updater also queries the car directly and requires Park and Sentry Mode off
before installation. The Sentry Mode check is omitted when a configured
keep-awake provider uses Sentry Mode (`SENTRY_CASE=1` or `2`); Park and archive
reachability are still required. Normal archive keep-awake cleanup runs before
these final checks. Drive, Reverse, Neutral, an unknown state or a failed BLE query
skips the update until the next qualifying archive. Unpaired installs skip the
BLE query. These checks cannot prevent movement after the final check or cancel
an installation once component replacement has started.

Automatic installation ignores the prerelease visibility setting. The target must be
a non-draft, non-prerelease GitHub release with a higher numeric version. For example,
an installed GitHub prerelease `v3.10.22` can update to stable `v4.0.0`, but will not
downgrade to stable `v3.9.9` or reinstall `v3.10.22`.

Archive completion keeps its normal notification timing. Installation uses the update
notification category: "Update v4.0.0 available. Installing now." After a verified reboot,
the outcome is "Updated to v4.0.0 successfully." Errors and interrupted attempts produce
a short update error notification, with detailed diagnostics retained locally.
Undelivered terminal outcomes are retained for retry, per provider, when connectivity returns.
Installation notices are also retried while the attempt is active; stale installation notices
expire when that attempt finishes or is replaced.
All update messages respect notification settings. Provider acceptance cannot guarantee
delivery to a phone, and a lost provider response can lead to a duplicate retry.

Installation restarts the device and briefly disconnects its USB drive. It needs free
space for staged files and recovery copies. A failed or interrupted installation is
left for the next qualifying archive rather than resumed at boot. Manual update
controls are available once startup recovery completes.

If an automatic-update recovery record is damaged, automatic installation stays
blocked. An explicitly requested manual update preserves that record for diagnosis
and can repair the installation.

### Armbian hotspot (Netplan)

On Armbian images using Netplan with systemd-networkd, Apply & Run Setup installs a
dedicated hotspot service and DHCP configuration for `ap0`. Devices upgrading from a
version without Armbian Netplan support need to apply their AP settings once. The home
Wi-Fi configuration is preserved; Away Mode temporarily releases that connection and
restores it when stopped or when hotspot startup fails. This backend is selected only on
Armbian with a Netplan Wi-Fi service and without NetworkManager enabled or running.
Other network backends use their existing setup paths.

The Armbian hotspot keeps DHCP leases in memory so it can assign addresses even when
networkd's persistent storage is read-only. Already-configured Armbian hotspots pick
this up automatically when Away Mode starts after an update or restart. A small
runtime override is recreated on each boot, so no setup rerun, AP setting change or
manual disabling of Away Mode is needed.

Away Mode shows "Starting" until the hotspot is ready, including the networkd DHCP
listener on Armbian. A failed start clears its active flag and displays the error;
Automatic mode retries with a fresh away location after a delay. Wi-Fi driver/AP
support is required.

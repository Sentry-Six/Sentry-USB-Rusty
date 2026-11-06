<h1 align="center">Sentry USB</h1>

<p align="center">
  <strong>Turn a Raspberry Pi into a smart USB drive for your Tesla's dashcam.</strong><br>
  Auto-archives. Modern web UI. Multi-camera viewer. Privacy-first.
</p>

<p align="center">
  <a href="https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Sentry-Six/Sentry-USB-Rusty"></a>
  <a href="https://discord.gg/9QZEzVwdnt"><img alt="Discord" src="https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg"></a>
</p>

<p align="center">
  <img src="docs/images/hero-dashboard.png" alt="Sentry USB dashboard" width="900">
</p>

---

## What it does

- **Plugs into your Tesla's USB port** and pretends to be a dashcam drive.
- **Tracks every drive**: route on a map, distance, speed, and the dashcam clips that recorded it, built from the metadata Tesla embeds in each clip.
- **Enriches drives with Tesla BLE telemetry**: battery, HVAC, cabin and exterior temps, TPMS, odometer, and location, pulled over Bluetooth and layered onto each trip for a much fuller picture than the dashcam metadata gives you on its own.
- **Archives clips automatically** to your NAS, cloud, or wherever, over WiFi in the background.
- **Keeps the car awake** (and the dashcam recording) via the same BLE link, with no Tesla API subscription needed.
- **Privacy controls.** Minimal device reporting with a visible setup choice and an easy opt-out.

The Rust rewrite of the original Go version. Same `sentryusb.conf`, faster server, more reliable.

Sentry USB is one of the free tools for Tesla owners from the [Sentry Six](https://sentry-six.com) project, alongside [Sentry Studio](https://github.com/Sentry-Six/Sentry-Six) (desktop TeslaCam viewer), [Sentry Drive](https://github.com/Sentry-Six/Sentry-Drive) (drive history and FSD analytics), and [Sentry Connect](https://sentry-six.com/sentry-connect/) (the iPhone companion app for this Pi). Not affiliated with Tesla, Inc. Unrelated to the Sentry (sentry.io) error-monitoring service.

> [!IMPORTANT]
> Beginning with update 2026.20, Tesla has begun offering the ability to encrypt Tesla cam footage, which is enabled by default. Encrypted files cannot be read by Sentry USB. To continue using Sentry USB, please disable the encryption.
> 
> You can find the relevant settings in your vehicle's menu under Safety, beneath the dashcam USB storage options.

---

## Install

**On a Raspberry Pi (recommended):**

1. Download **[Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest](https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/latest/download/Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest)** and double-click it to open [Raspberry Pi Imager](https://www.raspberrypi.com/software/).
2. Pick your Pi, add your Wi-Fi when Imager asks, and write the card.
3. Boot the Pi and open `http://sentryusb.local` about a minute later.
4. Follow the setup wizard, then plug the Pi into your Tesla.

The [Getting Started guide](https://github.com/Sentry-Six/Sentry-USB-Rusty/wiki/Getting-Started) has the click-by-click walkthrough.

**Other boards, or a Pi already running Raspberry Pi OS Lite (64-bit):**

```bash
sudo apt update && sudo apt upgrade -y
sudo -i
curl -fsSL https://usb.sentry-six.com | bash
```

> [!WARNING]
> The `apt update && apt upgrade` step is required. Pi OS images bake in an apt cache from whenever the image was built; if Debian has shipped with a newer version, the install will hit `404 Not Found` errors. Running the update and upgrade command will prevent this issue.

Then open `http://sentryusb.local` in a browser and the setup wizard takes you the rest of the way.

### Hardware

| Tier | Boards | Notes |
|------|--------|-------|
| **Recommended** | Raspberry Pi 4B, Raspberry Pi 5 | 1 GB RAM is sufficient. USB 2.0 OTG, fastest archiving, smoothest UI |
| **Tested** | Raspberry Pi Zero 2 W, Raspberry Pi 3A+ | USB 2.0 OTG. Works fine, slower archive speeds. Pi 3A+ needs a USB-A-to-USB-A cable |
| **Community** | Radxa Rock 4C+, Radxa Zero 3W | USB 3.0 OTG. Reported working, not officially supported |
| **Not supported** | Raspberry Pi 3B/3B+, Pi 2, Pi Zero W (v1), Pi 1 | 3B/3B+/2/1: USB goes through a hub chip, so they're host-only and can never appear as a drive to the car. Zero W v1: too slow, build retired |

Plus a **high-quality MicroSD card sized for the storage layout you choose** and a **USB 3.0 data cable** (not charge-only) between the Pi and your Tesla. Use a 3.0 cable even on USB 2.0 boards: it delivers more power and keeps lower-end Pis stable.

---

## Screenshots

<p align="center">
  <img src="docs/images/drives-list.png" alt="Drives list" width="900"><br>
  <em>Every drive your Tesla recorded (route, distance, duration, battery deltas, and FSD usage) with rolling totals for the selected time range.</em>
</p>

<p align="center">
  <img src="docs/images/drive-detail.png" alt="Drive detail" width="900"><br>
  <em>Per-drive deep view: speed curve, battery deltas, climate with HVAC runtime, odometer, and FSD analytics, alongside the route map and synchronized multi-camera dashcam playback.</em>
</p>

<p align="center">
  <img src="docs/images/charging-list.png" alt="Charging history" width="900"><br>
  <em>Every charge session (energy added, duration, cost, and charging speed) with per-tag time-of-use electricity rates and rolling totals for the selected range.</em>
</p>

<p align="center">
  <img src="docs/images/setup-wizard.png" alt="Setup Wizard" width="900"><br>
  <em>11-step setup wizard. No SSH, no config files.</em>
</p>

<p align="center">
  <img src="docs/images/settings.png" alt="Settings" width="900"><br>
  <em>Everything reconfigurable from the browser.</em>
</p>

---

## Features

| | |
|---|---|
| **Drives tracking** | Every trip with route, distance, duration, battery deltas, and FSD usage, plus the dashcam clips one click in |
| **Charging history** | Every charge session with energy added, duration, cost and charging speed, plus per-tag time-of-use rates |
| **Tesla BLE telemetry** | Adds battery, HVAC, cabin/exterior temps, TPMS, odometer, and location to each drive, beyond what Tesla's dashcam metadata carries on its own |
| **Multi-camera viewer** | Synchronized 6-camera playback with HW3-aware adaptive grid and drift correction |
| **Auto-archive** | CIFS / SMB, rsync, rclone (cloud), NFS. Kicks off whenever the Pi sees known WiFi |
| **Keep awake** | BLE (free, same link as telemetry), TeslaFi, Tessie, or generic webhook |
| **Sentry Cloud (beta)** | Encrypted cloud sync: drives are encrypted on the Pi before they leave your network |
| **Notifications** | Pushover, ntfy, Gotify, Discord, Telegram, Slack, Signal, Matrix, AWS SNS, IFTTT, Webhook, Sentry Connect app |
| **AI Support & Help** | In-app support chat; a diagnostics report is uploaded only after you approve it |
| **Privacy controls** | Minimal device statistics, a visible setup choice, and full per-flow disclosure |

---

## Status light

After Linux boots, Sentry USB uses the Pi's existing activity light to show startup, USB connection, camera-disk writes and application faults. The Pi's firmware boot and error codes remain unchanged. Different colours are for **Pi 5 only**; other supported boards use the same blink patterns and meanings.

See the [status-light wiki guide](https://github.com/Sentry-Six/Sentry-USB-Rusty/wiki/Status-LED) for **live blinking examples**, the status table and limitations.

---

## Privacy

Your dashcam footage and drive history stay on the Pi unless you turn on a feature that sends them somewhere, such as archiving to your NAS or cloud storage, or Sentry Cloud sync (encrypted on the Pi first). Some things go online in normal use: update checks go to GitHub, and maps in the web UI load tiles from `tiles.sentryusb.com`. Optional features contact Sentry Six services when you use them: device counting (if you choose **Yes, count me** during setup), automatic time zone detection, phone notifications, AI Support & Help, and wraps or lock chime submissions.

Full per-flow details are in [`wiki/Privacy.md`](wiki/Privacy.md), and the legal disclosures are in the [Privacy Policy](https://sentry-six.com/privacy).

---

## Documentation

- **[Wiki](https://github.com/Sentry-Six/Sentry-USB-Rusty/wiki)**: Getting Started, Setup Wizard Guide, Archive Methods, Notifications, Privacy, Troubleshooting, FAQ
- **[BUILD.md](BUILD.md)**: building from source

---

## Build from source

Prerequisites: Rust stable (1.89+), Node 20.19+ (or 22.12+), and `cross` (`cargo install cross`).

```bash
# Web UI
cd web && npm ci && npm run build && cd ..
rm -rf crates/sentryusb/static && cp -r web/dist crates/sentryusb/static

# Binaries (aarch64)
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb

# 32-bit (armhf, for Pi 3A+ with 32-bit Pi OS):
cross build --release --target armv7-unknown-linux-gnueabihf -p sentryusb
```

See [BUILD.md](BUILD.md) for details.

### Build the SD card image (advanced)

```bash
sudo image/build-image.sh --tag v3.29.0   # Linux, arm64 host
```

CI builds and attaches the image to each stable release; see [BUILD.md](BUILD.md#sd-card-image).

---

## Based on

A Rust rewrite of the original Go [`Scottmg1/Sentry-USB`](https://github.com/Scottmg1/Sentry-USB) (archived), itself a modernized fork of [TeslaUSB](https://github.com/marcone/teslausb) by marcone and contributors.

## Community

- **Discord**: [discord.gg/9QZEzVwdnt](https://discord.gg/9QZEzVwdnt), fastest help, real humans
- **Issues**: [github.com/Sentry-Six/Sentry-USB-Rusty/issues](https://github.com/Sentry-Six/Sentry-USB-Rusty/issues), for reproducible bugs
- **Site**: [sentry-six.com/sentry-usb](https://sentry-six.com/sentry-usb/)

## License

PolyForm Noncommercial 1.0.0: free for any noncommercial use; commercial use
requires a separate license. See [LICENSE](LICENSE). Some bundled shell scripts
are derived from [TeslaUSB](https://github.com/marcone/teslausb) and remain
under the MIT License; see [NOTICE](NOTICE) for the file-by-file breakdown.

This is source-available software, not open source: commercial use is not
permitted under this license. To inquire about a commercial license, contact
the authors.

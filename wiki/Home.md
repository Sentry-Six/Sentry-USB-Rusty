# Sentry USB Wiki

Turn a Raspberry Pi into a smart USB drive for your Tesla's dashcam.
Clips archive themselves. A web UI lets you review, search, and back
them up — no SSH, no config files.

## Install

**On a Raspberry Pi:** download **[Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest](https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/latest/download/Sentry-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest)**, double-click it to open [Raspberry Pi Imager](https://www.raspberrypi.com/software/), add your Wi-Fi when Imager asks, and write the card. Boot the Pi, wait about a minute, then open `http://sentryusb.local`. Full step-by-step in [Getting Started](Getting-Started).

**On other boards, or a Pi already running Pi OS**, use the one-liner:

```bash
sudo apt update && sudo apt upgrade -y
sudo -i
curl -fsSL https://usb.sentry-six.com | bash
```

> Refresh the apt cache first — Pi OS images bake in package lists that go stale as Debian publishes point releases, and a stale cache makes the install hit `404` errors.

Then open `http://sentryusb.local` and the [Setup Wizard](Setup-Wizard-Guide) takes you the rest of the way.

## What you need

| Tier | Boards | Notes |
|------|--------|-------|
| **Recommended** | Raspberry Pi 4B, Raspberry Pi 5 | 1 GB RAM is sufficient. USB 2.0 OTG — fastest archiving, smoothest UI |
| **Tested** | Raspberry Pi Zero 2 W, Raspberry Pi 3A+ | USB 2.0 OTG — works fine, slower archive speeds. Pi 3A+ needs a USB-A-to-USB-A cable |
| **Community** | Radxa Rock 4C+, Radxa Zero 3W | USB 3.0 OTG — reported working, not officially supported |
| **Not supported** | Raspberry Pi 3B/3B+, Pi 2, Pi Zero W (v1), Pi 1 | 3B/3B+/2/1: USB goes through a hub chip — host-only, can never appear as a drive to the car. Zero W v1: too slow, build retired |

Plus:
- A high-quality MicroSD card sized for the USB-drive layout and snapshot retention you choose
- A **USB 3.0 data cable** (not charge-only) — Pi to your Tesla.
  Use a 3.0 cable even on boards that only support USB 2.0 OTG: 3.0 cables
  deliver more power, which keeps lower-end boards stable.
- WiFi network with internet (for first-time setup and updates)

## Quick Links

| Page | What's on it |
|------|--------------|
| [Getting Started](Getting-Started) | Install in 10 minutes |
| [Rock 4C+ Setup](Rock-4C-Plus-Setup) | Install on a Radxa Rock 4C+ |
| [Setup Wizard Guide](Setup-Wizard-Guide) | Every wizard step explained |
| [Drives](Drives) | Trip tracking — route, distance, FSD usage, per-drive telemetry |
| [Tesla BLE Telemetry](Tesla-BLE-Telemetry) | What BLE pulls from the car and how it enriches drives |
| [Archive Methods](Archive-Methods) | CIFS, rsync, rclone, NFS |
| [Notifications](Notifications) | Push notifications to your phone |
| [Privacy](Privacy) | What we send, when, and why — and how to opt out |
| [Sentry Cloud](Sentry-Cloud) | Free drive and charging history, encrypted on your Pi |
| [Updates](Updates) | Manual and automatic updates |
| [Troubleshooting](Troubleshooting) | Things that go wrong |
| [Status Light](Status-LED) | What the Pi's blinking light means |
| [FAQ](FAQ) | Common questions |

## Links

- **Site**: [sentry-six.com/sentry-usb](https://sentry-six.com/sentry-usb/)
- **Sentry Cloud**: [sentryusb.com](https://sentryusb.com)
- **GitHub**: [Sentry-Six/Sentry-USB-Rusty](https://github.com/Sentry-Six/Sentry-USB-Rusty)
- **Releases**: [Latest](https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/latest)
- **Discord**: [Community chat](https://discord.gg/9QZEzVwdnt)
- **License**: PolyForm Noncommercial 1.0.0

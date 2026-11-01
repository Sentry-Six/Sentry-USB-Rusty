# Getting Started

Total time: roughly **10–15 minutes** of hands-on work, plus the time it takes to download and flash Pi OS.

> **Before you start:** check the [supported boards](Home#what-you-need). Notably, the **Pi 3B/3B+ cannot work at all** (hardware limitation), and a Pi 3A+ needs a USB-A-to-USB-A cable.

> [!NOTE]
> **There's no prebuilt Sentry USB SD image yet.** To be released.
> <br> For now, manual install is the only one supported. Requires a flash of stock Raspberry Pi OS Lite first, then run the installer over SSH. A bundled image will come later — for now this is the only supported install path.

## Manual Install

### 1. Flash the SD card

You'll use **Raspberry Pi Imager** to write Pi OS to your microSD card.

1.  Download [Raspberry Pi Imager](https://www.raspberrypi.com/software/) and open it.
2.  Insert your microSD card into your computer.
3.  On the **Device** step, pick your Pi model, then click **Next**.
4.  On the **OS** step, pick **Raspberry Pi OS (other)** → **Raspberry Pi OS Lite (64-bit)**, then click **Next**.
5.  On the **Storage** step, pick your SD card, then click **Next**.

Imager then walks you through the customization steps. Fill in each one and click **Next** (don't click **Skip customisation**):

*   **Hostname**: **leave it blank.** Sentry USB sets its own hostname during install.
*   **Localisation**: pick your nearest **capital city**. This also sets your time zone, keyboard layout and WiFi country.
*   **User**: set a **username** and **password**. Write them down.
*   **Wi-Fi**: choose **Secure network**, then enter your WiFi name (**SSID**) and password.
*   **Remote access**: turn on **Enable SSH** → **Use password authentication**.
*   Leave any other steps (such as **Raspberry Pi Connect**) as they are and click **Next**.

On the **Writing** step, click **Write**, then **I understand, erase and write**.

### 2. Boot the Pi

1.  Eject the SD card from your computer and put it into the Pi.
2.  Power on the Pi with any USB power supply. (Later you'll move the Pi to the car and power it from the Tesla using the same port.)
3.  Wait about 60 seconds for it to boot and join your WiFi.

### 3. Find the Pi's IP address

Open your router's admin page in a browser. The address is usually **http://192.168.1.1** or **http://192.168.0.1** — check the sticker on the bottom of your router.

Log in and find the device list (sometimes called "Connected Devices", "DHCP Clients", or "LAN Status"). Look for a device named **raspberrypi** and note its IP address (something like `192.168.1.47`).

### 4. SSH in and install

From your computer's terminal (Terminal on Mac, PowerShell on Windows):

```bash
ssh <your-username>@<the-IP-from-step-3>
```

Type the password you set in Pi Imager.

Once you're in, run:

```bash
sudo apt update && sudo apt upgrade -y
sudo -i
curl -fsSL https://usb.sentry-six.com | bash
```

> [!IMPORTANT]
> **Don't skip the `apt update && apt upgrade` step.** Pi OS images carry an apt cache from whenever the image was built. If Debian has published a point release since then, the cache points at `.deb` files that no longer exist on the mirrors and you'll see `404 Not Found` errors mid-install. The upgrade can take a couple of minutes — that's normal.

The installer itself then takes 2–5 minutes. It downloads the Sentry USB binary, sets up the system service, installs mDNS, and renames the Pi to `sentryusb`. Your SSH session may drop near the end when the hostname changes — that's expected.

### 5. Open the web UI

Open your browser and go to:

> **http://sentryusb.local**

The [Setup Wizard](Setup-Wizard-Guide) will walk you through the rest — picking your archive method, configuring notifications, etc.

> [!TIP]
> Keep the IP address the installer printed handy. Setup reboots the Pi several times, and `sentryusb.local` can briefly stop resolving after a reboot on some networks — if the page doesn't reconnect, open `http://<ip>` instead and setup picks up right where it left off.

### 6. Connect to your Tesla

After you finish the Setup Wizard:

1.  Power down the Pi (run `sudo poweroff` over SSH, then unplug it from your power supply).
2.  Plug your USB 3.0 cable into your Tesla's **glovebox USB port** (newer Teslas) or one of the **front USB ports** (older Teslas).
3.  Plug the other end into the Pi.
4.  The Pi boots from the car's power. Within a few seconds, your dashcam icon will appear and start recording to the Pi.

> [!IMPORTANT]
> Tesla introduced dashcam encryption in update 2026.20. At the moment, there is no way to decrypt the files on-device. To use Sentry USB, please disable "Encrypt Dashcam Recordings" in the settings. This feature is only available on AMD-equipped vehicles.
> <br>
> <br> **To find the toggle, navigate to:**
> <br> Settings → Safety → Dashcam

## Need help?

*   [Troubleshooting](Troubleshooting) — common install issues
*   [FAQ](FAQ)
*   [Discord](https://discord.gg/9QZEzVwdnt) — fastest answers


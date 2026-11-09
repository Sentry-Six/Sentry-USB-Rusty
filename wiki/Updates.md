# Updates

Sentry USB updates itself from its [GitHub releases](https://github.com/Sentry-Six/Sentry-USB-Rusty/releases). Updates need the Pi to be on Wi-Fi with internet.

## Update by hand

Open the web UI and go to **Settings → Device → Software Updates**, then click **Check for Updates**. If a new version is available, you can install it from there.

Under **Update Preferences** you can also choose:

- **Check after archiving**: look for a new version after each archive. This only checks; it doesn't install anything.
- **Include pre-releases**: also offer test builds. These may have bugs.

## Install updates automatically

Turn on **Install stable updates automatically** under **Settings → Device → Update Preferences**. It's off until you turn it on.

When it's on, Sentry USB installs a new **stable** release right after an archive that copied new footage, which normally means the car is parked at home. Before it installs, it checks that:

- the archive is still reachable, and
- if Tesla BLE is paired, the car is in **Park** with **Sentry Mode off**. (If your keep-awake method uses Sentry Mode, only Park is checked.)

If a check fails, it waits for the next archive and tries again.

Good to know:

- Automatic updates only install stable releases newer than yours. They skip pre-releases and never go back to an older version.
- Installing restarts the Pi and briefly disconnects its USB drive from the car.
- You get an update notification when the install starts and another when it finishes or fails (see [Notifications](Notifications)).
- If an install fails, Sentry USB tries again after the next archive that copies new footage. You can also update by hand at any time.

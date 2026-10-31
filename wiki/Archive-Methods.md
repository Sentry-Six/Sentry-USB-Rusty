# Archive Methods

Pick whichever fits how you already store stuff. Sentry USB archives your dashcam clips, sentry events, and (optionally) saved clips to one of four backends.

## Archive only driving Recent Clips

In **Settings → Setup Wizard → Archive → What to Archive**, enable **Recent Clips**
and **Recent Clips: only while driving**, then apply the configuration.

This keeps all available camera angles for a recording when its embedded Tesla
metadata contains a non-Park gear sample or a speed sample above the configured
**Speed threshold**. Enter the threshold in your preferred km/h or mph units;
changing units preserves the physical speed. The default is 1.8 km/h (about
1.118 mph), equivalent to 0.5 m/s. Gear evidence qualifies independently of this threshold. Clips
that include both driving and parking are kept in full, including stops in Drive.
Parked-only clips, missing metadata, and unreadable recordings are skipped.
This applies to every archive backend. Saved, Sentry, and Track Mode selections
remain independent. Existing archived files are not removed.

The configuration setting is `ARCHIVE_RECENTCLIPS_DRIVING_ONLY=true` alongside
`ARCHIVE_RECENTCLIPS=true`. It defaults to `false` for existing installations.
The threshold is stored in metres per second as `ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS`
and accepts finite, non-negative values, including zero.

For manual edits, convert mph to m/s by multiplying by `0.44704`, or divide
km/h by `3.6`. For example, **5 mph = 2.2352 m/s**:

```bash
export ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS=2.2352
```

You do not need long decimal values: `2.24` is a practical approximation of
5 mph. The Setup Wizard handles these conversions automatically.

Reading metadata adds work before transfers, especially on lower-powered Pis.
Successful camera-file scans are cached in the `archive_driving_evidence` table
of the shared `/backingfiles/drive-data.db`, including parked and metadata-free
results. Subsequent cycles check file metadata
and reuse evidence for unchanged files instead of reading their video again.
Changing the speed threshold reuses the stored gear and maximum-speed evidence.
New or changed files are scanned; read failures are retried on the next cycle.
Results are committed as each scan finishes so reboots or interrupted archives
retain completed work. Entries are pruned when files leave the archive candidate
list. Existing results from `/mutable/archive-driving-cache.sqlite` are imported
once, transactionally, without replacing newer shared entries. The old cache is
left unchanged for rollback. Installations without a previous cache build it on
the first cycle.

## CIFS / SMB

For Windows file sharing, macOS file sharing, and most consumer NAS devices (Synology, QNAP, TrueNAS).

**On your server:** create a shared folder, give a user read/write access, note the share name.

**In the wizard:**

| Field | Example |
|-------|---------|
| Archive Server | `192.168.1.100` or `nas.local` |
| Share Name | `TeslaCam` or `media/dashcam` |
| Username | `tesla` |
| Password | _your password_ |
| CIFS Version | leave blank unless you know you need 2.0 / 1.0 |
| Unraid SMB compatibility | enable only for Unraid shares reporting `Stale file handle` |

If the connection fails, the most common cause is an older NAS that needs `CIFS_VERSION=2.0` set explicitly.

For Unraid shares that report `Stale file handle`, enable **Unraid SMB compatibility**
in **Settings → Setup Wizard → Archive**, then apply the configuration. This saves
`CIFS_NOSERVERINO=true` and adds `noserverino` to the archive and music SMB mounts,
including connection tests. It survives reboots, updates, and setup reruns. It is
off by default and has no effect on NFS, rsync, or rclone.

`noserverino` tells the Pi to generate file IDs locally instead of relying on the
server's IDs, which can change when Unraid moves files between storage locations.
The client cannot reliably identify hard links with this option. Disable the
setting and apply setup again to restore the default SMB behavior.

After a failed transfer in normal mode, Sentry USB retries automatically after
60 seconds, then 120, 240, and at most 300 seconds between attempts while the server
remains reachable. Archive Sync can start an attempt sooner. A successful transfer
or a departure resets the delay; cancelled transfers still wait for departure.
Travel Mode keeps its existing interval and Fast Retry setting. Repeated retry
failures are logged without repeating push notifications.

## rsync

For Linux/Unix servers. Faster and more reliable than CIFS over the open internet, but requires SSH key setup.

**On your server:** create a user, create a destination folder.

**In the wizard:**

| Field | Example |
|-------|---------|
| Server | `archive.example.com` |
| Username | `tesla` |
| Remote Path | `/home/tesla/dashcam` |
| SSH Port | leave blank unless your server's SSH daemon listens on a port other than 22 |

**After the wizard finishes**, you need to copy the Pi's SSH public key to the server. SSH into the Pi:

```bash
ssh-copy-id <username>@<server>

# if your server uses a custom SSH port:
ssh-copy-id -p <port> <username>@<server>
```

You'll only have to do this once.

## rclone

For cloud storage — Google Drive, OneDrive, Dropbox, S3, Backblaze B2, and ~60 other providers. Best for offsite backups.

**Set up the remote first** by SSH'ing into the Pi and running:

```bash
sudo rclone config
```

(Run it as root — the archive loop reads `/root/.config/rclone/rclone.conf`.)

Follow the prompts — it'll ask which cloud service, walk you through OAuth, and let you name the remote (e.g., `gdrive`).

**In the wizard:**

| Field | Example |
|-------|---------|
| Remote Name | `gdrive` (matches the name you set in `rclone config`) |
| Remote Path | `Dashcam` or `Backups/TeslaCam` |

## NFS

For Linux NAS devices that prefer NFS over CIFS (some Synology and TrueNAS setups). **Typically faster than CIFS / SMB** on the same hardware — worth using if you have a lot of clips to back up and your NAS supports it.

**On your server:** export a directory in `/etc/exports` (or your NAS's GUI), allow the Pi's IP.

**In the wizard:**

| Field | Example |
|-------|---------|
| NFS Server | `192.168.1.100` |
| Export Path | `/volume1/TeslaCam` (the exact path from your `exports` file) |

NFS is unauthenticated — anyone on your LAN with the path can read/write. Use CIFS or rsync if that's a concern.

## Switching methods later

You can re-run the [Setup Wizard](Setup-Wizard-Guide) from **Settings** to switch backends. Already-archived clips stay where they are — Sentry USB doesn't re-archive past clips, only future ones go to the new destination.

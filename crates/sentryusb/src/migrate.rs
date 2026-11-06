//! Startup migration: update peripheral files (shell scripts, BLE daemon,
//! Avahi service, etc.) when the binary has been replaced by a newer version
//! but the surrounding artifacts on disk are stale.
//!
//! This solves the bootstrap problem for existing installs whose Rust binary
//! was updated via a minimal replace-only update path — their scripts, BLE
//! daemon, and service files were left at the old version. Once this code
//! has run once, future boots will self-heal automatically.
//!
//! Gated by a marker file (`/opt/sentryusb/.migrated-<version>`) so it runs
//! at most once per installed version. Never touches user setup configuration.

use std::time::Duration;

use tracing::{info, warn};

const VERSION_FILE: &str = "/opt/sentryusb/version";
const MIGRATE_DIR: &str = "/opt/sentryusb";
const MIGRATE_REPO: &str = "Sentry-Six/Sentry-USB-Rusty";
const MIGRATE_BRANCH: &str = "main";
const REPAIR_TOOL_SELECTOR: &str = include_str!("../../../run/xfs-repair-tool.py");

/// Optional upgrade preparation, separate from the required migration's
/// timeout. This only caches a command; it never starts filesystem repair.
async fn prepare_upgrade_repair_tool(selector: &str, timeout: Duration) -> bool {
    match sentryusb_shell::run_with_timeout(
        timeout, "python3", &["-c", selector, "--cache-persistent"],
    ).await {
        Ok(_) => {
            info!("[migrate] Compatible XFS repair tool prepared for offline recovery");
            true
        }
        Err(error) => {
            warn!("[migrate] XFS repair tool preparation deferred: {error}. Setup or repair can retry; recovery may need internet access until the tool is cached.");
            false
        }
    }
}

async fn finish_migration(
    marker_file: &str,
    preparation: impl std::future::Future<Output = bool> + Send + 'static,
) -> bool {
    if let Err(error) = tokio::fs::write(marker_file, b"migrated\n").await {
        warn!("[migrate] Failed to write marker {marker_file}: {error}");
        return false;
    }
    // Settle required migration independently. Waiting for an optional
    // download here would leave an automatic update in `verifying`, so a
    // restart during that wait could incorrectly mark the update failed.
    tokio::spawn(preparation);
    true
}

pub async fn run_startup_migration() -> bool {
    // Skip in dev mode (no version file, or explicit "dev")
    let current_version = match tokio::fs::read_to_string(VERSION_FILE).await {
        Ok(v) => v.trim().to_string(),
        Err(_) => return false,
    };
    if current_version.is_empty() || current_version == "dev" {
        return false;
    }

    let automatic = match sentryusb_api::update_attempt::load(&sentryusb_api::update_attempt::current_path()) {
        Ok(Some(a)) if !a.terminal() => {
            if a.phase != "awaiting_reboot" || a.target != current_version { return false; }
            if let Err(e) = sentryusb_api::auto_update::prepare_startup_verification() {
                warn!("Automatic update did not complete an expected shutdown: {e:#}");
                return false;
            }
            true
        }
        Ok(Some(a)) if a.phase != "verified" && a.target == current_version => {
            warn!("Previous automatic update failed; not replaying its migration on a later boot");
            return false;
        }
        Ok(_) => false,
        Err(e) => { warn!("Cannot verify automatic update receipt: {e:#}"); return false; }
    };
    let marker_file = format!("{}/.migrated-{}", MIGRATE_DIR, current_version);
    if !automatic && tokio::fs::metadata(&marker_file).await.is_ok() {
        return true;
    }

    info!("[migrate] Running startup migration for {}...", current_version);

    // Prefer the exact version tag; fall back to the tracking branch if missing.
    let script_ref = if current_version == "unknown" {
        MIGRATE_BRANCH.to_string()
    } else {
        current_version.clone()
    };
    let tarball_url = format!(
        "https://github.com/{}/archive/{}.tar.gz",
        MIGRATE_REPO, script_ref
    );

    let script = migration_script_for_mode(&tarball_url, &script_ref, automatic);

    // Retry up to 3 times with exponential backoff. The script itself
    // fails fast on `curl: Could not resolve host: github.com` when DNS
    // isn't ready yet — and that's exactly the state we hit racing
    // network-online.target at boot. Every retry is a full script run;
    // `set -e` + idempotent file writes mean re-running after a partial
    // success is safe (existing files get overwritten with identical
    // bytes from the tarball).
    //
    // The `nss-lookup.target` dependency on the service unit is the
    // primary fix; this is belt-and-suspenders for the edge case where
    // the resolver comes up but its first query fails because the
    // upstream DNS cache is cold.
    let mut last_err: Option<String> = None;
    for attempt in 1..=3 {
        match sentryusb_shell::run_with_timeout(
            Duration::from_secs(180),
            "bash",
            &["-c", &script],
        )
        .await
        {
            Ok(_) => {
                // Re-apply runtime patches AFTER the migration. The migration
                // script unconditionally rewrites /root/bin/sentryusb-ble.py
                // from the upstream tarball — which silently undoes board-
                // specific fixes the OTA updater already applied (BCM4345C0
                // non-fatal-adv on Rock 4C+ is the headline case: OTA patches
                // ble.py → reboot → this migration unpatches it → BLE crash
                // loop on next start). Invoke the standalone runtime-patches
                // script (idempotent + detection-gated) so the patches survive
                // every migration. Best-effort: a missing script just yields
                // an info log — the OTA flow's bootstrap path will populate
                // it on the next update.
                if std::path::Path::new("/usr/local/bin/sentryusb-apply-runtime-patches").exists() {
                    match sentryusb_shell::run_with_timeout(
                        Duration::from_secs(30),
                        "/usr/local/bin/sentryusb-apply-runtime-patches",
                        &[],
                    )
                    .await
                    {
                        Ok(_) => info!("[migrate] runtime-patches re-applied post-migration"),
                        Err(e) => { warn!(
                            "[migrate] runtime-patches post-migration run failed: {} — BLE pairing may be broken on Rock 4C+",
                            e
                        ); if automatic { return false; } },
                    }
                } else {
                    info!("[migrate] runtime-patches script not present (pre-bootstrap install) — skipping; OTA path will populate it");
                }

                // Existing installations don't rerun setup after an update.
                // The migration has made the root card writable, so prepare
                // the embedded release's tool now, for both update modes.
                // Startup migration already runs in a background task; the
                // UI keeps starting, and an unavailable download must not
                // turn an otherwise successful update into a failed one.
                let _ = tokio::fs::create_dir_all(MIGRATE_DIR).await;
                if !finish_migration(&marker_file, prepare_upgrade_repair_tool(
                    REPAIR_TOOL_SELECTOR, Duration::from_secs(240),
                )).await {
                    return false;
                }
                info!("[migrate] Startup migration complete for {}", current_version);
                return true;
            }
            Err(e) => {
                let msg = e.to_string();
                // Only retry for transient failure signatures. A genuine
                // 404 on the tarball URL, a write permission error, or
                // an archive-corrupt error isn't going to fix itself on
                // a second try, and we don't want to burn 30+ seconds
                // on guaranteed-failing retries.
                let transient = msg.contains("Could not resolve host")
                    || msg.contains("Temporary failure in name resolution")
                    || msg.contains("Connection timed out")
                    || msg.contains("Network is unreachable");
                if attempt < 3 && transient {
                    let wait = Duration::from_secs(5 * attempt as u64);
                    warn!(
                        "[migrate] Startup migration attempt {}/3 hit a transient failure ({}); retrying in {:?}",
                        attempt, msg, wait
                    );
                    tokio::time::sleep(wait).await;
                    last_err = Some(msg);
                    continue;
                }
                last_err = Some(msg);
                break;
            }
        }
    }
    warn!(
        "[migrate] Warning: startup migration failed after retries: {}",
        last_err.as_deref().unwrap_or("unknown")
    );
    // Don't write marker — retry on next boot.
    false
}

fn migration_script_for_mode(tarball_url: &str, release_ref: &str, automatic: bool) -> String {
    let mut script = build_migration_script(tarball_url, release_ref);
    if automatic {
        // The payload was validated/staged with the target binary. Never fetch
        // main or resume a partial installation after an unexpected power cut.
        let start = script.find("# Download repo tarball").unwrap();
        let end = script[start..].find("# ── Update run/ scripts").map(|n| n + start).unwrap();
        script.replace_range(start..end, "tar xzf /opt/sentryusb/auto-update-source.tar.gz --strip-components=1 -C \"$TMPDIR\"\n\n");
        script.insert_str(0, "SENTRYUSB_AUTOMATIC_UPDATE=1\n");
    }

    script
}

fn build_migration_script(tarball_url: &str, release_ref: &str) -> String {
    let helper_release_base = if release_ref == MIGRATE_BRANCH {
        format!("https://github.com/{MIGRATE_REPO}/releases/latest/download")
    } else {
        format!("https://github.com/{MIGRATE_REPO}/releases/download/{release_ref}")
    };
    format!(
        r#"set -e

# Remount filesystem as read-write (no-op if already rw)
/root/bin/remountfs_rw 2>/dev/null || mount -o remount,rw / 2>/dev/null || true

# Ensure /root/bin exists — on fresh Rust installs it isn't created by setup,
# so cp targets below would otherwise fail.
mkdir -p /root/bin

# ── Migrate broken cttseraser FUSE fstab entry to bind mount ──
# Installs that completed setup before the FUSE → bind-mount switch still
# carry the legacy `mount.ctts#/mutable/TeslaCam …` fstab line, which
# depends on a /sbin/mount.ctts helper and the cttseraser binary —
# both removed in the bind-mount switch. With the helper gone the mount
# unit fails on every boot and /var/www/html/TeslaCam stays empty,
# returning 404 for every clip in the dashboard player. The setup
# wizard rewrites the fstab on first run, but already-set-up installs
# never re-run it, so they need this one-shot migration instead.
# Idempotent: skipped entirely when no `mount.ctts#` entry exists.
if grep -qE '^[^#]*mount\.ctts#' /etc/fstab 2>/dev/null; then
  # Strip the legacy entry AND any prior bind-mount line targeting the
  # same path, then append the canonical bind entry. awk + mv is atomic
  # so a power loss mid-write leaves /etc/fstab in its previous state.
  awk '
    /^[^#]*mount\.ctts#/ {{ next }}
    /^[^#]*\/var\/www\/html\/TeslaCam[[:space:]]+none[[:space:]]+bind/ {{ next }}
    {{ print }}
  ' /etc/fstab > /etc/fstab.new
  echo '/mutable/TeslaCam /var/www/html/TeslaCam none bind,nofail,x-systemd.requires=/mutable 0 0' >> /etc/fstab.new
  mv /etc/fstab.new /etc/fstab
  systemctl daemon-reload 2>/dev/null || true
  # Clear the failed state from the legacy FUSE mount unit so the new
  # bind-mount unit (same name, generated from the new fstab entry by
  # systemd-fstab-generator) can activate without the residual error.
  systemctl reset-failed var-www-html-TeslaCam.mount 2>/dev/null || true
  systemctl start var-www-html-TeslaCam.mount 2>/dev/null || true
  echo "migrate: replaced legacy mount.ctts# fstab entry with bind mount"
fi

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

# Download repo tarball — try version tag first, fall back to tracking branch
if ! curl -fsSL "{tarball_url}" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null; then
  FALLBACK="https://github.com/{repo}/archive/{branch}.tar.gz"
  curl -fsSL "$FALLBACK" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null || exit 1
fi

# ── Update run/ scripts ──
if [ -d "$TMPDIR/run" ]; then
  for f in "$TMPDIR"/run/*; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    staged=$(mktemp "/root/bin/.$name.XXXXXX")
    cp "$f" "$staged"
    chmod +x "$staged"
    mv -f "$staged" "/root/bin/$name"
  done
fi

# ── Update archive module scripts ──
ARCHIVE_SYSTEM=""
for conf in /root/sentryusb.conf /sentryusb/sentryusb.conf; do
  if [ -f "$conf" ]; then
    ARCHIVE_SYSTEM=$(grep -m1 'ARCHIVE_SYSTEM=' "$conf" 2>/dev/null | tail -1 | sed "s/.*ARCHIVE_SYSTEM=//;s/['\"]//g;s/#.*//" | tr -d ' ') || true
    [ -n "$ARCHIVE_SYSTEM" ] && break
  fi
done
if [ -n "$ARCHIVE_SYSTEM" ]; then
  subdir="${{ARCHIVE_SYSTEM}}_archive"
  if [ -d "$TMPDIR/run/$subdir" ]; then
    for f in "$TMPDIR/run/$subdir"/*; do
      [ -f "$f" ] || continue
      name=$(basename "$f")
      staged=$(mktemp "/root/bin/.$name.XXXXXX")
      cp "$f" "$staged"
      chmod +x "$staged"
      mv -f "$staged" "/root/bin/$name"
    done
  fi
fi

# ── Update setup-sentryusb (kept as compatibility wrapper) ──
if [ -f "$TMPDIR/setup/pi/setup-sentryusb" ]; then
  cp "$TMPDIR/setup/pi/setup-sentryusb" "/root/bin/setup-sentryusb"
  chmod +x "/root/bin/setup-sentryusb"
fi

# ── Update envsetup.sh (kept as compatibility wrapper) ──
if [ -f "$TMPDIR/setup/pi/envsetup.sh" ]; then
  cp "$TMPDIR/setup/pi/envsetup.sh" "/root/bin/envsetup.sh"
  chmod +x "/root/bin/envsetup.sh"
fi

# ── Update BLE peripheral daemon (binary and/or Python fallback) ──
if [ -f "$TMPDIR/server/ble/sentryusb-ble.py" ]; then
  cp "$TMPDIR/server/ble/sentryusb-ble.py" "/root/bin/sentryusb-ble.py"
  chmod +x "/root/bin/sentryusb-ble.py"
fi
if [ -f "$TMPDIR/server/ble/sentryusb-ble.service" ]; then
  cp "$TMPDIR/server/ble/sentryusb-ble.service" "/etc/systemd/system/sentryusb-ble.service"
  systemctl daemon-reload
fi

# ── Install BLE Python dependencies if missing ──
for pkg in python3-dbus python3-gi bluez; do
  if ! dpkg-query -W --showformat='${{db:Status-Status}}\n' "$pkg" 2>/dev/null | grep -q '^installed$'; then
    DEBIAN_FRONTEND=noninteractive apt-get -y install "$pkg" 2>/dev/null || true
  fi
done

# ── Configure bluetoothd --experimental by BlueZ version ──
# Legacy BlueZ (< 5.55) needs it for LEAdvertisingManager1; newer BlueZ does not, and
# the flag there registers LE Audio services that trigger Android pairing prompts.
BTOVERRIDE=/etc/systemd/system/bluetooth.service.d/sentryusb-experimental.conf
BTDAEMON=$(systemctl cat bluetooth.service 2>/dev/null | grep '^ExecStart=' | head -1 | sed 's/ExecStart=//' | awk '{{print $1}}')
BTDAEMON=${{BTDAEMON:-$(command -v bluetoothd 2>/dev/null)}}
if [ -n "$BTDAEMON" ] && [ -x "$BTDAEMON" ]; then
  BTVER=$("$BTDAEMON" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+' | head -1)
  BTMAJ=${{BTVER%%.*}}
  BTMIN=${{BTVER##*.}}
  if [ -n "$BTVER" ] && {{ [ "$BTMAJ" -lt 5 ] || {{ [ "$BTMAJ" -eq 5 ] && [ "$BTMIN" -lt 55 ]; }}; }}; then
    if [ ! -f "$BTOVERRIDE" ]; then
      mkdir -p /etc/systemd/system/bluetooth.service.d
      cat > "$BTOVERRIDE" << BTEOF
[Service]
ExecStart=
ExecStart=$BTDAEMON --experimental
BTEOF
      systemctl daemon-reload
      systemctl restart bluetooth 2>/dev/null || true
      sleep 2
    fi
  elif [ -f "$BTOVERRIDE" ]; then
    rm -f "$BTOVERRIDE"
    systemctl daemon-reload
    systemctl restart bluetooth 2>/dev/null || true
    sleep 2
  fi
fi

# ── Install/update Avahi mDNS service ──
if [ -f "$TMPDIR/setup/pi/avahi-sentryusb.service" ]; then
  if ! dpkg -s avahi-daemon >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -qq avahi-daemon avahi-utils >/dev/null 2>&1 || true
  fi
  if dpkg -s avahi-daemon >/dev/null 2>&1; then
    mkdir -p /etc/avahi/services
    # Same rendering as setup and sentryusb-ble.py: the suffix TXT record is
    # the last 4 characters of /etc/machine-id, uppercased.
    _machine_id=$(tr -d '[:space:]' < /etc/machine-id 2>/dev/null || true)
    _avahi_suffix=$(printf '%s' "$_machine_id" | tail -c 4 | tr '[:lower:]' '[:upper:]')
    # Restart avahi only when the service file or daemon config changed.
    _avahi_changed=0
    _avahi_service=/etc/avahi/services/sentryusb.service
    sed "s/@DEVICE_SUFFIX@/${{_avahi_suffix:-0000}}/" "$TMPDIR/setup/pi/avahi-sentryusb.service" \
      > "$_avahi_service.new"
    if cmp -s "$_avahi_service.new" "$_avahi_service"; then
      rm -f "$_avahi_service.new"
    else
      mv -f "$_avahi_service.new" "$_avahi_service"
      _avahi_changed=1
    fi
    # IPv4-only mDNS advertising (stale-AAAA slowness + Chrome PNA "CORS"
    # blocks on http://sentryusb.local) and no workstation record (keeps
    # legacy-unicast answers in one packet) — see the helper's header.
    # Nonfatal, but loud: the setup repair path is the self-heal backstop.
    if [ -f "$TMPDIR/setup/pi/avahi-ipv4-only.sh" ]; then
      if _avahi_conf=$(bash "$TMPDIR/setup/pi/avahi-ipv4-only.sh"); then
        if [ "$_avahi_conf" = changed ]; then _avahi_changed=1; fi
      else
        echo "migrate WARNING: could not apply avahi daemon config" >&2
      fi
    else
      echo "migrate WARNING: avahi-ipv4-only.sh missing from update payload" >&2
    fi
    systemctl enable avahi-daemon 2>/dev/null || true
    if [ "$_avahi_changed" = 1 ] || ! systemctl -q is-active avahi-daemon; then
      systemctl restart avahi-daemon 2>/dev/null || true
    fi
  fi
fi

# ── Migrate AP to Away Mode (AP off by default) ──
# Skip the teardown while an Away Mode session is active (flag file
# present): this migration runs unordered vs restore_from_file()'s boot
# AP start, so downing the profile here could strand a parked-away Pi
# until it returns home. Autoconnect-off is still safe to apply — Away
# Mode always raises the AP explicitly.
if nmcli -t con show SENTRYUSB_AP &>/dev/null; then
  nmcli con modify SENTRYUSB_AP connection.autoconnect no 2>/dev/null || true
  # The hotspot hands out no router or DNS, so phones keep their own
  # internet (setup installs the same file). NM reads it the next time the
  # hotspot starts; a running Away Mode session is left as it is.
  mkdir -p "$(dirname {ap_dnsmasq_conf})"
  cat > {ap_dnsmasq_conf}.new << 'APDNSMASQEOF'
{ap_dnsmasq_contents}APDNSMASQEOF
  if cmp -s {ap_dnsmasq_conf}.new {ap_dnsmasq_conf}; then
    rm -f {ap_dnsmasq_conf}.new
  else
    mv -f {ap_dnsmasq_conf}.new {ap_dnsmasq_conf}
  fi
  if [ ! -f /mutable/sentryusb_away_mode.json ]; then
    nmcli con down SENTRYUSB_AP 2>/dev/null || true
    iw dev ap0 del 2>/dev/null || true
  fi
fi
# The hostapd/ifupdown hotspot's dnsmasq hands out no DNS server either (it
# never sent a router). Only a setup-written config is touched; dnsmasq reads
# it the next time Away Mode starts the hotspot.
if grep -qxF 'interface=lo,ap0' /etc/dnsmasq.conf 2>/dev/null \
  && grep -qxF 'no-dhcp-interface=lo,wlan0' /etc/dnsmasq.conf \
  && grep -qxF 'dhcp-option=3' /etc/dnsmasq.conf \
  && ! grep -qxF '{hostapd_no_dns}' /etc/dnsmasq.conf; then
  printf '%s\n' '# or a DNS server, so clients keep their own' '{hostapd_no_dns}' >> /etc/dnsmasq.conf
fi
WLAN=$(nmcli -t -f TYPE,DEVICE c show --active 2>/dev/null | grep 802-11-wireless | grep -v ':ap0$' | cut -d: -f2 | head -1)
WLAN=${{WLAN:-wlan0}}
if [ -d /etc/NetworkManager/dispatcher.d ]; then
  cat > /etc/NetworkManager/dispatcher.d/10-sentryusb-ap << APEOF
#!/bin/bash
IFACE="\$1"
ACTION="\$2"
if [ "\$IFACE" = "$WLAN" ] && [ "\$ACTION" = "up" ]; then
  if [ -f /mutable/sentryusb_away_mode.json ]; then
    if ! iw dev ap0 info &> /dev/null; then
      iw dev $WLAN interface add ap0 type __ap || true
    fi
    iw $WLAN set power_save off 2>/dev/null || true
    iw ap0 set power_save off 2>/dev/null || true
    nmcli con up SENTRYUSB_AP 2>/dev/null || true
  fi
fi
APEOF
  chmod 755 /etc/NetworkManager/dispatcher.d/10-sentryusb-ap
fi

# ── Refresh the per-CPU binary picker ──
# The picker now also re-validates the telemetry + ble-action symlinks
# under /root/bin at every boot (the issue #88 SIGILL self-heal). Ship
# the new version to existing installs from the tarball. Harmless on
# very old installs whose sentryusb.service predates the ExecStartPre
# hook — the script just sits unused until the unit is updated.
if [ -f "$TMPDIR/pi-gen-sources/00-sentryusb-tweaks/files/sentryusb-pick-binary" ]; then
  install -m 755 "$TMPDIR/pi-gen-sources/00-sentryusb-tweaks/files/sentryusb-pick-binary" /usr/local/bin/sentryusb-pick-binary
fi

# ── Refresh the runtime-patches helper from the tarball ──
# Without this, new patches we add to apply-runtime-patches.sh never reach
# existing installs — the OTA invocation (the Rust caller after this shell
# script exits) would re-run the OLD on-disk version. Bootstrap pre-v3.11
# installs that never had the helper at all (the file just appears).
if [ -f "$TMPDIR/setup/pi/apply-runtime-patches.sh" ]; then
  install -m 755 "$TMPDIR/setup/pi/apply-runtime-patches.sh" /usr/local/bin/sentryusb-apply-runtime-patches
fi

# ── Install Tesla BLE telemetry sampler service + aux binaries ──
# Two concerns here:
#   1. The systemd unit ships in the source tarball — copy it into place
#      and enable it. Idempotent on every upgrade.
#   2. The telemetry + ble-action binaries need to be on disk in the
#      picker-managed layout: per-CPU variant copies under
#      /opt/sentryusb with /root/bin/<name> as symlinks the picker
#      re-validates at every boot (issue #88). A legacy regular file at
#      /root/bin is converted by re-downloading the right variant — its
#      own variant is unknowable (it may be the wrong-CPU build that
#      caused the SIGILL crash-loop in the first place), so we never
#      trust it. The old file is only replaced after a successful
#      download; offline installs keep working and convert on a later
#      run. Already-converted installs (symlink present) are skipped.
if [ -f "$TMPDIR/server/ble/sentryusb-telemetry.service" ]; then
  cp "$TMPDIR/server/ble/sentryusb-telemetry.service" "/etc/systemd/system/sentryusb-telemetry.service"
  systemctl daemon-reload
  systemctl enable sentryusb-telemetry 2>/dev/null || true

  # Match the suffix scheme update.rs uses. Three-tier: active-variant
  # file (written by sentryusb-pick-binary) → live CPU detection → arch
  # family fallback. The aarch64 suffix is per-CPU (-a53/-a72/-a76) so
  # the aux binaries' tuning matches the main daemon's tuning.
  # Only trust active-variant when it's a real release suffix — old
  # pickers recorded their on-disk fallback (or "legacy") here, and a
  # download URL built from that installs the wrong CPU variant.
  _suffix=""
  if [ -s /opt/sentryusb/active-variant ]; then
    _suffix=$(cat /opt/sentryusb/active-variant 2>/dev/null | tr -d '[:space:]')
    case "$_suffix" in
      linux-arm64-a53|linux-arm64-a72|linux-arm64-a76|linux-armv7|linux-amd64) ;;
      *) _suffix="" ;;
    esac
  fi
  if [ -z "$_suffix" ]; then
    _arch=$(dpkg --print-architecture 2>/dev/null || true)
    case "$_arch" in
      armhf)  _suffix=linux-armv7 ;;
      amd64)  _suffix=linux-amd64 ;;
      arm64)
        # Sub-detect for aarch64: HWCAP atomics+aes → a76, CPU part 0xD08
        # → a72, else a53. Same rules as sentryusb-pick-binary (the a76
        # build keeps the crypto extension, so it needs the aes hwcap).
        if grep -qE '^Features.*\batomics\b' /proc/cpuinfo 2>/dev/null \
          && grep -qE '^Features.*\baes\b' /proc/cpuinfo 2>/dev/null; then
          _suffix=linux-arm64-a76
        elif grep -qE '^CPU part[[:space:]]*:[[:space:]]*0x[dD]08' /proc/cpuinfo 2>/dev/null; then
          _suffix=linux-arm64-a72
        else
          _suffix=linux-arm64-a53
        fi
        ;;
      *)
        _arch=$(uname -m 2>/dev/null || echo "")
        case "$_arch" in
          armv7l)  _suffix=linux-armv7 ;;
          x86_64)  _suffix=linux-amd64 ;;
          aarch64)
            if grep -qE '^Features.*\batomics\b' /proc/cpuinfo 2>/dev/null \
              && grep -qE '^Features.*\baes\b' /proc/cpuinfo 2>/dev/null; then
              _suffix=linux-arm64-a76
            elif grep -qE '^CPU part[[:space:]]*:[[:space:]]*0x[dD]08' /proc/cpuinfo 2>/dev/null; then
              _suffix=linux-arm64-a72
            else
              _suffix=linux-arm64-a53
            fi
            ;;
          *)       _suffix="" ;;
        esac
        ;;
    esac
  fi
  if [ -n "$_suffix" ]; then
    for _name in sentryusb-tesla-telemetry sentryusb-ble-action; do
      _dest="/opt/sentryusb/$_name-$_suffix"
      _link="/root/bin/$_name"
      # Skip only a healthy link to the expected executable. A dangling
      # symlink satisfies -L but execve returns ENOENT; older code skipped it
      # forever and still wrote the per-version migration marker.
      if [ -L "$_link" ] \
        && [ "$(readlink "$_link" 2>/dev/null || true)" = "$_dest" ] \
        && [ -x "$_link" ] \
        && [ -x "$_dest" ]; then
        continue
      fi
      if [ "${{SENTRYUSB_AUTOMATIC_UPDATE:-0}}" = 1 ]; then
        echo "migrate: staged automatic-update helper missing: $_name" >&2
        exit 1
      fi
      _url="{helper_release_base}/$_name-$_suffix"
      _staged="/opt/sentryusb/.$_name.migrate.new"
      mkdir -p /root/bin /opt/sentryusb
      rm -f "$_staged"
      # Stage on the destination filesystem so the final rename is atomic.
      # Any failure aborts the migration, so Rust does not write .migrated-*;
      # the next boot can retry instead of preserving a half-install.
      curl -fsSL --max-time 120 "$_url" -o "$_staged"
      [ -s "$_staged" ]
      chmod +x "$_staged"
      mv -f "$_staged" "$_dest"
      ln -sfn "$_dest" "$_link"
      [ -x "$_link" ]
      echo "migrate: installed $_name-$_suffix and linked $_link"
    done
  fi

  # Only attempt restart if the binary is actually present — the
  # ConditionPathExists in the unit would otherwise log a confusing
  # "skipped" line on every upgrade where BLE isn't set up yet.
  if [ -x /root/bin/sentryusb-tesla-telemetry ]; then
    systemctl restart sentryusb-telemetry 2>/dev/null || true
  fi
fi

# Clean up the pre-v6 lock path if it's still around from an older
# awake_start. The new path is /tmp/ble_radio_owner; leaving the old
# file lying around looks like a held lock to the new code.
rm -f /tmp/ble_keep_awake_active 2>/dev/null || true

# ── Restart BLE daemon ──
systemctl enable sentryusb-ble 2>/dev/null || true
systemctl restart sentryusb-ble 2>/dev/null || true

# ── Post-migration patches (persist across upstream script updates) ──
# These re-apply user-facing fixes onto the run/ scripts just replaced with
# upstream copies. Idempotent — the `grep -q` guards prevent re-patching.

# Patch 1: send-push-message — respect SENTRY_NOTIFICATION_URL
if grep -q 'https://notifications.sentry-six.com/send"' /root/bin/send-push-message 2>/dev/null; then
  sed -i 's|"https://notifications.sentry-six.com/send"|"${{SENTRY_NOTIFICATION_URL:-https://notifications.sentry-six.com}}/send"|' /root/bin/send-push-message
fi

# Patch 2: archiveloop — read active chime from library dir instead of flat file
if grep -q '[ -f "/mutable/LockChime.wav" ]' /root/bin/archiveloop 2>/dev/null; then
  python3 - <<'PYEOF'
content = open('/root/bin/archiveloop').read()
old = '    if [ -f "/mutable/LockChime.wav" ]\n    then\n      cp -f "/mutable/LockChime.wav" "$CAM_MOUNT/LockChime.wav"'
new = '    _active_chime=$(cat /mutable/LockChime/.active_name 2>/dev/null || true)\n    if [ -n "$_active_chime" ] && [ -f "/mutable/LockChime/$_active_chime" ]\n    then\n      cp -f "/mutable/LockChime/$_active_chime" "$CAM_MOUNT/LockChime.wav"'
if old in content:
    open('/root/bin/archiveloop','w').write(content.replace(old, new, 1))
PYEOF
fi

# ── Remove obsolete tesla-control / tesla-keygen ──
# Pairing, keygen and every Tesla BLE command are native now
# (sentryusb-ble-action + the tesla_ble crate); nothing references the
# external Go binaries anymore. They're ~28 MB of dead weight on the
# space-tight root partition. The telemetry unit copied + reloaded above
# no longer gates on tesla-control, so removing them can't break startup.
# Idempotent.
rm -f /root/bin/tesla-control /root/bin/tesla-keygen \
      /usr/local/bin/tesla-control /usr/local/bin/tesla-keygen 2>/dev/null || true
"#,
        tarball_url = tarball_url,
        repo = MIGRATE_REPO,
        helper_release_base = helper_release_base,
        branch = MIGRATE_BRANCH,
        ap_dnsmasq_conf = sentryusb_setup::network::ap_profile::AP_DNSMASQ_CONF,
        ap_dnsmasq_contents = sentryusb_setup::network::ap_profile::AP_DNSMASQ_CONTENTS,
        hostapd_no_dns = sentryusb_setup::network::HOSTAPD_DNSMASQ_NO_DNS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn required_migration_finishes_while_optional_preparation_is_pending() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("sentryusb-upgrade-cache-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("migrated");
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let (finished, mut completion) = tokio::sync::oneshot::channel();
        let preparation = async move {
            let _ = wait.await;
            let _ = finished.send(());
            false // Even a failed optional download cannot undo completion.
        };
        let result = tokio::time::timeout(Duration::from_secs(1),
            finish_migration(marker.to_str().unwrap(), preparation)).await;
        assert!(matches!(result, Ok(true)), "optional preparation held required migration completion");
        assert_eq!(std::fs::read(&marker).unwrap(), b"migrated\n");
        assert!(matches!(completion.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        release.send(()).unwrap();
        completion.await.unwrap();
        assert!(marker.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_migration_marker_does_not_start_optional_preparation() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let started = Arc::new(AtomicBool::new(false));
        let observed = started.clone();
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let missing = std::env::temp_dir().join(format!("sentryusb-missing-{}-{nonce}/migrated", std::process::id()));
        assert!(!finish_migration(missing.to_str().unwrap(), async move {
            observed.store(true, Ordering::SeqCst);
            true
        }).await);
        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn upgrade_prepares_persistent_cache_without_requesting_repair() {
        let selector = "import sys\nassert sys.argv[1:] == ['--cache-persistent'], sys.argv\nprint('prepared')\n";
        assert!(prepare_upgrade_repair_tool(selector, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn upgrade_cache_download_failure_is_nonfatal() {
        let selector = "import sys\nprint('download unavailable', file=sys.stderr)\nsys.exit(1)\n";
        assert!(!prepare_upgrade_repair_tool(selector, Duration::from_secs(5)).await);
        // Failure returns to the migration caller instead of aborting it.
        assert!(prepare_upgrade_repair_tool("print('retry succeeded')", Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn upgrade_cache_preparation_has_a_bounded_timeout() {
        let started = std::time::Instant::now();
        assert!(!prepare_upgrade_repair_tool("import time; time.sleep(10)", Duration::from_millis(100)).await);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn automatic_migration_uses_cached_payload_and_preserves_manual_fallback() {
        let automatic = migration_script_for_mode("https://example.invalid/release.tar.gz", "v0.0.0", true);
        assert!(automatic.starts_with("SENTRYUSB_AUTOMATIC_UPDATE=1\n"));
        assert!(automatic.contains("tar xzf /opt/sentryusb/auto-update-source.tar.gz"));
        assert!(!automatic.contains("https://example.invalid/release.tar.gz"));
        assert!(!automatic.contains("FALLBACK="));
        let mut child = std::process::Command::new("bash").arg("-n")
            .stdin(std::process::Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(automatic.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        let manual = migration_script_for_mode("https://example.invalid/release.tar.gz", "v0.0.0", false);
        assert!(manual.contains("https://example.invalid/release.tar.gz"));
        assert!(manual.contains("FALLBACK="));
        assert!(!manual.starts_with("SENTRYUSB_AUTOMATIC_UPDATE=1"));
    }

    /// The migration script is a format!() template full of shell — a
    /// stray unescaped `{` or a typo'd quote renders a script that fails
    /// at runtime on every user's Pi. Render it and let bash parse it.
    #[test]
    fn migration_script_parses() {
        let script = build_migration_script(
            "https://github.com/Sentry-Six/Sentry-USB-Rusty/archive/v0.0.0.tar.gz",
            "v0.0.0",
        );
        // Placeholders must all have been substituted.
        assert!(!script.contains("{repo}"), "unsubstituted {{repo}}");
        assert!(!script.contains("{branch}"), "unsubstituted {{branch}}");
        assert!(!script.contains("{tarball_url}"), "unsubstituted {{tarball_url}}");

        let dir = std::env::temp_dir().join("sentryusb-migrate-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("migration.sh");
        std::fs::write(&path, &script).unwrap();
        let status = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .status()
            .expect("bash not available");
        assert!(status.success(), "bash -n rejected the migration script");
    }

    #[test]
    fn migration_installs_the_hotspot_dnsmasq_conf_for_existing_nm_profiles() {
        use sentryusb_setup::network::ap_profile::{AP_DNSMASQ_CONF, AP_DNSMASQ_CONTENTS};
        let script = build_migration_script("https://example.invalid/source.tar.gz", "v0.0.0");
        let start = script.find("if nmcli -t con show SENTRYUSB_AP").unwrap();
        let end = start + script[start..].find("\nfi\n").unwrap() + "\nfi\n".len();

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("sentryusb-ap-migrate-{}-{nonce}", std::process::id()));
        let conf = root.join("dnsmasq-shared.d/sentryusb-ap.conf");
        let block = script[start..end].replace(AP_DNSMASQ_CONF, conf.to_str().unwrap());
        let harness = format!("set -eu\nnmcli() {{ :; }}\niw() {{ :; }}\n{block}");
        let run = || {
            let status = std::process::Command::new("bash").args(["-c", &harness]).status().unwrap();
            assert!(status.success());
        };

        run();
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), AP_DNSMASQ_CONTENTS);
        let written = std::fs::metadata(&conf).unwrap().modified().unwrap();
        run();
        assert_eq!(std::fs::metadata(&conf).unwrap().modified().unwrap(), written);
        assert!(!conf.with_extension("conf.new").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_stops_the_hostapd_dnsmasq_handing_out_dns_once() {
        let script = build_migration_script("https://example.invalid/source.tar.gz", "v0.0.0");
        let start = script.find("if grep -qxF 'interface=lo,ap0' /etc/dnsmasq.conf").unwrap();
        let end = start + script[start..].find("\nfi\n").unwrap() + "\nfi\n".len();

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("sentryusb-dnsmasq-migrate-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let conf = root.join("dnsmasq.conf");
        let block = script[start..end].replace("/etc/dnsmasq.conf", conf.to_str().unwrap());
        let run = || {
            let status = std::process::Command::new("bash").args(["-c", &format!("set -eu\n{block}")]).status().unwrap();
            assert!(status.success());
        };

        // A config from an earlier setup gains the line once.
        let before = "interface=lo,ap0\nno-dhcp-interface=lo,wlan0\nbind-interfaces\nbogus-priv\n\
            dhcp-range=192.168.66.10,192.168.66.254,12h\n\
            # don't configure a default route, we're not a router\ndhcp-option=3\n";
        std::fs::write(&conf, before).unwrap();
        run();
        run();
        let after = std::fs::read_to_string(&conf).unwrap();
        assert_eq!(after, format!("{before}# or a DNS server, so clients keep their own\n{}\n",
            sentryusb_setup::network::HOSTAPD_DNSMASQ_NO_DNS));

        // Any other dnsmasq.conf (or none) is left alone.
        let custom = "interface=eth0\ndhcp-range=10.0.0.10,10.0.0.50,12h\n";
        std::fs::write(&conf, custom).unwrap();
        run();
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), custom);
        std::fs::remove_file(&conf).unwrap();
        run();
        assert!(!conf.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_script_repairs_dangling_aux_links() {
        let script = build_migration_script(
            "https://github.com/Sentry-Six/Sentry-USB-Rusty/archive/v0.0.0.tar.gz",
            "v0.0.0",
        );

        assert!(
            script.contains("&& [ -x \"$_link\" ]") && script.contains("&& [ -x \"$_dest\" ]"),
            "auxiliary migration must not treat a dangling symlink as healthy"
        );
        assert!(
            script.contains("_staged=\"/opt/sentryusb/.$_name.migrate.new\""),
            "auxiliary downloads must be staged on the destination filesystem"
        );
        assert!(
            script.contains("[ -x \"$_link\" ]\n      echo \"migrate: installed"),
            "migration must verify the published executable before succeeding"
        );
    }

    #[test]
    fn migration_downloads_helpers_from_the_installed_release() {
        let script = build_migration_script(
            "https://github.com/Sentry-Six/Sentry-USB-Rusty/archive/v0.0.0.tar.gz",
            "v0.0.0",
        );

        assert!(
            script.contains(
                "https://github.com/Sentry-Six/Sentry-USB-Rusty/releases/download/v0.0.0/$_name-$_suffix"
            ),
            "migration must keep auxiliary binaries on the installed release"
        );
        assert!(
            !script.contains("releases/latest/download/$_name-$_suffix"),
            "a migration for an exact version must not install latest helpers"
        );
    }

    #[cfg(unix)]
    #[test]
    fn helper_repair_keeps_automatic_updates_offline_and_manual_updates_versioned() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let script = build_migration_script("https://example.invalid/source.tar.gz", "v1.2.3");
        let start = script.find("    for _name in sentryusb-tesla-telemetry sentryusb-ble-action; do").unwrap();
        let end = start + script[start..].find("\n    done").unwrap() + "\n    done".len();
        let loop_body = &script[start..end];

        for (automatic, healthy, download_ok) in [
            (true, true, true), (true, false, true),
            (false, false, true), (false, false, false),
        ] {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("sentryusb-helper-{}-{nonce}", std::process::id()));
            let install = root.join("install");
            let bin = root.join("bin");
            std::fs::create_dir_all(&install).unwrap();
            std::fs::create_dir_all(&bin).unwrap();
            for name in ["sentryusb-tesla-telemetry", "sentryusb-ble-action"] {
                let target = install.join(format!("{name}-linux-arm64-a53"));
                if healthy {
                    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
                }
                symlink(&target, bin.join(name)).unwrap();
            }
            let isolated = loop_body.replace("/opt/sentryusb", install.to_str().unwrap())
                .replace("/root/bin", bin.to_str().unwrap());
            let harness = format!(r#"set -eu
_suffix=linux-arm64-a53
curl() {{
    printf '%s\n' "$*" >> "$TEST_DIR/requests"
    [ "$DOWNLOAD_OK" = 1 ] || return 22
    while [ "$#" -gt 0 ]; do
        if [ "$1" = -o ]; then
            printf '#!/bin/sh\nexit 0\n' > "$2"
            return 0
        fi
        shift
    done
    return 2
}}
{isolated}
"#);
            let output = std::process::Command::new("bash").args(["-c", &harness])
                .env("TEST_DIR", &root)
                .env("SENTRYUSB_AUTOMATIC_UPDATE", if automatic { "1" } else { "0" })
                .env("DOWNLOAD_OK", if download_ok { "1" } else { "0" })
                .output().unwrap();
            assert_eq!(output.status.success(), healthy || (!automatic && download_ok), "{}", String::from_utf8_lossy(&output.stderr));
            let requests = std::fs::read_to_string(root.join("requests")).unwrap_or_default();
            if automatic {
                assert!(requests.is_empty(), "automatic migration downloaded a helper");
            } else {
                assert!(requests.contains("/releases/download/v1.2.3/"));
                assert!(!requests.contains("/latest/"));
                if download_ok {
                    for name in ["sentryusb-tesla-telemetry", "sentryusb-ble-action"] {
                        assert!(bin.join(name).is_file(), "missing repaired helper: {name}");
                    }
                }
            }
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}

//! WiFi AP configuration — port of `configure-ap.sh`.
//!
//! Sets up a concurrent AP on a virtual interface (ap0). Tries NetworkManager
//! first, falls back to writing the .nmconnection keyfile directly (needed
//! when NM was started on a read-only root and its keyfile plugin refuses
//! `nmcli con add`), and finally falls back to wpa_supplicant + hostapd on
//! systems without NetworkManager.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::info;

use crate::env::SetupEnv;
use crate::error::ConfigError;
use crate::SetupEmitter;

pub mod ap_profile;
pub mod networkd;

/// Configure the WiFi access point.
///
/// The runner gates this on `AP_SSID` and a valid `AP_PASS` being set, so by
/// the time we get here both are populated. We still defend against missing
/// values in case this is called directly.
pub async fn configure_ap(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    let ssid = match env.config.get("AP_SSID") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => {
            info!("AP_SSID not set, skipping AP configuration");
            return Ok(());
        }
    };

    let pass = match env.config.get("AP_PASS") {
        Some(v) if !v.is_empty() && v != "password" && v.len() >= 8 => v.clone(),
        _ => {
            return Err(ConfigError(
                "AP_PASS not set, unchanged from default, or too short (min 8 chars)".into(),
            )
            .into())
        }
    };

    emitter.begin_phase("wifi_ap", "WiFi access point");
    emitter.progress(&format!("Configuring WiFi AP: {}", ssid));

    let ip = env.get("AP_IP", "192.168.66.1");

    // NetworkManager path (by far the most common on modern Pi OS / Trixie).
    if sentryusb_shell::run("systemctl", &["--quiet", "is-enabled", "NetworkManager.service"])
        .await
        .is_ok()
    {
        // Make sure `iw` is installed — it would otherwise get swept up by
        // autoremove when alsa-utils is removed in the readonly phase.
        let _ = crate::apt::apt_install(
            |m| emitter.progress(m),
            &["iw"],
            Duration::from_secs(120),
        ).await;

        match nm_add_ap(&ssid, &pass, &ip, emitter).await {
            Ok(()) => {
                teardown_ap_scaffolding().await;
                emitter.progress("AP configured via NetworkManager.");
                return Ok(());
            }
            Err(e) => {
                info!("nmcli AP add failed ({e}); falling back to keyfile writer");
                // NM's keyfile plugin sometimes refuses `nmcli con add` when
                // NM was started while root was read-only. Writing the
                // .nmconnection file directly and loading it avoids a
                // service restart. Both paths verify the loaded profile.
                let result = nm_write_ap_file(&ssid, &pass, &ip, emitter).await
                    .with_context(|| format!("failed to configure AP (nmcli add failed: {e:#}; keyfile fallback also failed)"));
                teardown_ap_scaffolding().await;
                result?;
                emitter.progress("AP configured via keyfile fallback.");
                return Ok(());
            }
        }
    }

    // Netplan/networkd Armbian images do not have ifupdown's files or tools.
    if networkd::applicable().await? {
        return networkd::configure(env, emitter).await;
    }

    // wpa_supplicant + hostapd path (non-NM systems).
    if !Path::new("/etc/wpa_supplicant/wpa_supplicant.conf").exists() {
        emitter.progress("No wpa_supplicant, skipping AP setup.");
        return Ok(());
    }
    let current = std::fs::read_to_string("/etc/wpa_supplicant/wpa_supplicant.conf")
        .unwrap_or_default();
    if current.contains("id_str") {
        emitter.progress("AP mode already configured");
        return Ok(());
    }
    configure_hostapd_path(&ssid, &pass, &ip, emitter).await
}

/// Tear down the AP scaffolding the NM configure paths leave behind.
///
/// Setup only installs the connection profile — Away Mode owns bringing the
/// AP up. The ap0 interface created so `nmcli con add` succeeds must not
/// outlive setup: it pins the shared radio to the AP channel (hurting wlan0
/// scans) and its mere existence used to trigger archiveloop's `wifi_cycle`
/// into bringing the AP up with Away Mode off. The `con down` also covers NM
/// having auto-activated the profile during configuration. Skipped while an
/// Away Mode session is running so re-running setup doesn't kill the AP the
/// user is connected through.
async fn teardown_ap_scaffolding() {
    if Path::new("/mutable/sentryusb_away_mode.json").exists() {
        return;
    }
    let _ = sentryusb_shell::run("nmcli", &["con", "down", "SENTRYUSB_AP"]).await;
    let _ = sentryusb_shell::run("iw", &["dev", "ap0", "del"]).await;
}

/// Remove the WiFi AP configuration entirely.
///
/// Called when setup runs without AP settings, so unchecking "Enable WiFi
/// Access Point" in the wizard actually removes the feature instead of
/// silently leaving the old profile (and a possibly-broadcasting AP) behind.
pub async fn deconfigure_ap(emitter: &SetupEmitter) -> Result<()> {
    if networkd::applicable().await? {
        networkd::remove().await?;
        let _ = std::fs::remove_file("/mutable/sentryusb_away_mode.json");
        let _ = std::fs::remove_file("/mutable/sentryusb_away_mode.json.tmp");
        return Ok(());
    }
    let keyfile = "/etc/NetworkManager/system-connections/SENTRYUSB_AP.nmconnection";
    let dispatcher = "/etc/NetworkManager/dispatcher.d/10-sentryusb-ap";

    let profile_exists =
        sentryusb_shell::run("nmcli", &["-t", "con", "show", "SENTRYUSB_AP"]).await.is_ok();
    let ap0_exists = sentryusb_shell::run("iw", &["dev", "ap0", "info"]).await.is_ok();
    if !profile_exists && !ap0_exists && !Path::new(keyfile).exists() && !Path::new(dispatcher).exists() {
        return Ok(());
    }

    emitter.begin_phase("wifi_ap", "WiFi access point");
    emitter.progress("Removing WiFi access point configuration");

    // End any Away Mode session: with the profile gone the flag file would
    // only make the dispatcher and archiveloop chase an AP that no longer
    // exists.
    let _ = std::fs::remove_file("/mutable/sentryusb_away_mode.json");
    let _ = std::fs::remove_file("/mutable/sentryusb_away_mode.json.tmp");

    let _ = sentryusb_shell::run("nmcli", &["con", "down", "SENTRYUSB_AP"]).await;
    let _ = sentryusb_shell::run("nmcli", &["con", "delete", "SENTRYUSB_AP"]).await;
    // `con delete` can fail under the same read-only-root keyfile quirk the
    // add path works around — remove the file directly and reload.
    let _ = std::fs::remove_file(keyfile);
    let _ = sentryusb_shell::run("nmcli", &["con", "reload"]).await;

    let _ = sentryusb_shell::run("iw", &["dev", "ap0", "del"]).await;
    let _ = std::fs::remove_file(dispatcher);
    let _ = std::fs::remove_file("/etc/network/if-up.d/sentryusb-ap");

    emitter.progress("WiFi access point removed.");
    Ok(())
}

/// Primary NM path: `nmcli con add` + modifications.
async fn nm_add_ap(
    ssid: &str,
    pass: &str,
    ip: &str,
    emitter: &SetupEmitter,
) -> Result<()> {
    let wlan = find_wifi_device().await?;
    emitter.progress(&format!("WiFi client interface: {}", wlan));

    // Create virtual AP interface if it doesn't exist.
    if sentryusb_shell::run("iw", &["dev", "ap0", "info"]).await.is_err() {
        sentryusb_shell::run(
            "iw", &["dev", &wlan, "interface", "add", "ap0", "type", "__ap"],
        ).await.context("failed to create ap0 virtual interface")?;
    }

    // Disable power save on both interfaces (they share hardware, and we
    // don't want one to sleep just because the other is idle).
    let _ = sentryusb_shell::run("iw", &[&wlan, "set", "power_save", "off"]).await;
    let _ = sentryusb_shell::run("iw", &["ap0", "set", "power_save", "off"]).await;

    // Remove old / legacy connection names.
    let _ = sentryusb_shell::run("nmcli", &["con", "delete", "SENTRYUSB_AP"]).await;
    let _ = sentryusb_shell::run("nmcli", &["con", "delete", "TESLAUSB_AP"]).await;

    // autoconnect is set at add time: a profile created with the default
    // (autoconnect=yes) can be auto-activated by NM in the window before a
    // later `con modify`, leaving the AP broadcasting right out of setup.
    sentryusb_shell::run(
        "nmcli", &["con", "add", "type", "wifi", "ifname", "ap0", "mode", "ap",
                   "con-name", "SENTRYUSB_AP", "autoconnect", "no", "ssid", ssid],
    ).await.context("nmcli con add failed")?;

    sentryusb_shell::run(
        "nmcli", &["con", "modify", "SENTRYUSB_AP",
                   "802-11-wireless-security.key-mgmt", "wpa-psk"],
    ).await?;
    sentryusb_shell::run(
        "nmcli", &["con", "modify", "SENTRYUSB_AP",
                   "802-11-wireless-security.psk", pass],
    ).await?;
    sentryusb_shell::run(
        "nmcli", &["con", "modify", "SENTRYUSB_AP",
                   "ipv4.addr", &format!("{}/24", ip)],
    ).await?;
    sentryusb_shell::run(
        "nmcli", &["con", "modify", "SENTRYUSB_AP", "ipv4.method", "shared"],
    ).await?;
    sentryusb_shell::run(
        "nmcli", &["con", "modify", "SENTRYUSB_AP", "ipv6.method", "disabled"],
    ).await?;

    ap_profile::verify_nm_ap_profile(ssid, ip).await?;

    // Clean up stale if-up.d script from previous installs.
    let _ = std::fs::remove_file("/etc/network/if-up.d/sentryusb-ap");

    install_ap_dispatcher(&wlan).await?;
    Ok(())
}

/// Fallback: write the connection file directly, load it and verify it.
///
/// When NM's keyfile plugin started on a read-only root it refuses
/// `nmcli con add`, but once the FS is remounted rw we can write the
/// file ourselves. `nmcli con load` picks it up without a full restart,
/// so SSH sessions survive.
async fn nm_write_ap_file(
    ssid: &str,
    pass: &str,
    ip: &str,
    emitter: &SetupEmitter,
) -> Result<()> {
    emitter.progress("Writing AP connection file directly (NM keyfile workaround)");

    let wlan = find_wifi_device().await?;

    if sentryusb_shell::run("iw", &["dev", "ap0", "info"]).await.is_err() {
        sentryusb_shell::run(
            "iw", &["dev", &wlan, "interface", "add", "ap0", "type", "__ap"],
        ).await.context("failed to create ap0 virtual interface")?;
    }
    let _ = sentryusb_shell::run("iw", &[&wlan, "set", "power_save", "off"]).await;
    let _ = sentryusb_shell::run("iw", &["ap0", "set", "power_save", "off"]).await;

    let _ = sentryusb_shell::run("nmcli", &["con", "delete", "SENTRYUSB_AP"]).await;
    let _ = sentryusb_shell::run("nmcli", &["con", "delete", "TESLAUSB_AP"]).await;

    std::fs::create_dir_all("/etc/NetworkManager/system-connections")?;
    let file = "/etc/NetworkManager/system-connections/SENTRYUSB_AP.nmconnection";
    let contents = format!(
        "[connection]\n\
         id=SENTRYUSB_AP\n\
         type=wifi\n\
         interface-name=ap0\n\
         autoconnect=false\n\
         \n\
         [wifi]\n\
         mode=ap\n\
         ssid={ssid}\n\
         \n\
         [wifi-security]\n\
         key-mgmt=wpa-psk\n\
         psk={pass}\n\
         \n\
         [ipv4]\n\
         address1={ip}/24\n\
         method=shared\n\
         \n\
         [ipv6]\n\
         method=disabled\n"
    );
    // Created 0600 from the start (no world-readable window for the
    // PSK). NM's keyfile plugin REFUSES to load connection files with
    // looser perms, so getting this wrong doesn't just leak — it
    // silently breaks the AP profile.
    write_secret_file(file, &contents)?;

    ap_profile::load_nm_ap_profile(ssid, ip).await?;

    let _ = std::fs::remove_file("/etc/network/if-up.d/sentryusb-ap");
    install_ap_dispatcher(&wlan).await?;
    Ok(())
}

/// hostapd + dnsmasq path — for systems that don't use NetworkManager.
async fn configure_hostapd_path(
    ssid: &str,
    pass: &str,
    ip: &str,
    emitter: &SetupEmitter,
) -> Result<()> {
    emitter.progress("installing dnsmasq and hostapd");
    crate::apt::apt_install(
        |m| emitter.progress(m),
        &["dnsmasq", "hostapd"],
        Duration::from_secs(300),
    ).await.context("failed to install hostapd/dnsmasq")?;

    emitter.progress(&format!("configuring AP '{ssid}' with IP {ip}"));

    let net = ip.rsplit_once('.').map(|(p, _)| p).unwrap_or("192.168.66");

    let mac = std::fs::read_to_string("/sys/class/net/wlan0/address")
        .unwrap_or_default();
    let mac = mac.trim();

    // udev rule — creates ap0 on hardware phy0 and pins its MAC.
    let udev_rule = format!(
        "SUBSYSTEM==\"ieee80211\", ACTION==\"add|change\", \
         ATTR{{macaddress}}==\"{mac}\", KERNEL==\"phy0\", \
         RUN+=\"/sbin/iw phy phy0 interface add ap0 type __ap\", \
         RUN+=\"/bin/ip link set ap0 address {mac}\"\n"
    );
    let _ = std::fs::create_dir_all("/etc/udev/rules.d");
    std::fs::write("/etc/udev/rules.d/70-persistent-net.rules", udev_rule)?;

    std::fs::write(
        "/etc/dnsmasq.conf",
        format!(
            "interface=lo,ap0\n\
             no-dhcp-interface=lo,wlan0\n\
             bind-interfaces\n\
             bogus-priv\n\
             dhcp-range={net}.10,{net}.254,12h\n\
             # don't configure a default route, we're not a router\n\
             dhcp-option=3\n"
        ),
    )?;

    let _ = std::fs::create_dir_all("/etc/hostapd");
    // 0600 — the file carries the WPA passphrase.
    write_secret_file(
        "/etc/hostapd/hostapd.conf",
        &format!(
            "ctrl_interface=/var/run/hostapd\n\
             ctrl_interface_group=0\n\
             interface=ap0\n\
             driver=nl80211\n\
             ssid={ssid}\n\
             hw_mode=g\n\
             channel=11\n\
             wmm_enabled=0\n\
             macaddr_acl=0\n\
             auth_algs=1\n\
             wpa=2\n\
             wpa_passphrase={pass}\n\
             wpa_key_mgmt=WPA-PSK\n\
             wpa_pairwise=TKIP CCMP\n\
             rsn_pairwise=CCMP\n"
        ),
    )?;

    std::fs::write(
        "/etc/default/hostapd",
        "DAEMON_CONF=\"/etc/hostapd/hostapd.conf\"\n",
    )?;

    std::fs::write(
        "/etc/network/interfaces",
        format!(
            "source-directory /etc/network/interfaces.d\n\
             \n\
             auto lo\n\
             auto ap0\n\
             auto wlan0\n\
             iface lo inet loopback\n\
             \n\
             allow-hotplug ap0\n\
             iface ap0 inet static\n\
             \x20\x20\x20\x20address {ip}\n\
             \x20\x20\x20\x20netmask 255.255.255.0\n\
             \x20\x20\x20\x20hostapd /etc/hostapd/hostapd.conf\n\
             \n\
             allow-hotplug wlan0\n\
             iface wlan0 inet manual\n\
             \x20\x20\x20\x20wpa-roam /etc/wpa_supplicant/wpa_supplicant.conf\n\
             iface AP1 inet dhcp\n"
        ),
    )?;

    // Bullseye needs wpa_supplicant explicitly disabled on ap0.
    let dhcpcd_line = "# disable wpa_supplicant for the ap0 interface\n\
                       interface ap0\n\
                       nohook wpa_supplicant\n";
    append_unless_contains("/etc/dhcpcd.conf", "nohook wpa_supplicant", dhcpcd_line)?;

    // Migrate /var/lib/misc to /mutable so the dnsmasq lease file persists
    // across reboots when root is read-only.
    if !Path::new("/var/lib/misc").is_symlink() {
        if sentryusb_shell::run("findmnt", &["--mountpoint", "/mutable"])
            .await
            .is_err()
        {
            let _ = sentryusb_shell::run("mount", &["/mutable"]).await;
        }
        let _ = std::fs::create_dir_all("/mutable/varlib");
        let _ = sentryusb_shell::run("mv", &["/var/lib/misc", "/mutable/varlib/"]).await;
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("/mutable/varlib/misc", "/var/lib/misc");
    }

    // Update hosts so the sentryusb mDNS name resolves to the AP IP rather
    // than 127.0.0.1 for clients connected to the AP.
    if let Ok(hosts) = std::fs::read_to_string("/etc/hosts") {
        let new: String = hosts
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("127.0.0.1")
                    && !l.trim_start().starts_with("127.0.0.1\tlocalhost")
                    && !l.trim_start().starts_with("127.0.0.1 localhost")
                {
                    // Replace 127.0.0.1 prefix with the AP IP; keep the rest.
                    let rest = l
                        .trim_start()
                        .strip_prefix("127.0.0.1")
                        .unwrap_or(l);
                    format!("{}{}", ip, rest)
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write("/etc/hosts", new + "\n")?;
    }

    // Tag the wpa_supplicant network block(s) with the id_str the
    // ifupdown config maps to AP1. Only `network={...}` blocks are
    // touched — a bare `.replace("}")` would also stamp cred / p2p
    // blocks and anything else with a closing brace.
    if let Ok(conf) = std::fs::read_to_string("/etc/wpa_supplicant/wpa_supplicant.conf") {
        let new = tag_network_blocks_with_id_str(&conf, "AP1");
        std::fs::write("/etc/wpa_supplicant/wpa_supplicant.conf", new)?;
    }

    emitter.progress("AP configured via hostapd/wpa_supplicant");
    Ok(())
}

/// Insert `id_str="<id>"` as the last entry of every `network={...}`
/// block. Other block types (`cred={`, `p2p_...`) and stray braces are
/// left untouched. Blocks that already carry an id_str are skipped, so
/// the transform is idempotent.
fn tag_network_blocks_with_id_str(conf: &str, id: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_network = false;
    let mut block_has_id_str = false;
    for line in conf.lines() {
        let trimmed = line.trim();
        if !in_network && trimmed.starts_with("network={") {
            in_network = true;
            block_has_id_str = false;
        } else if in_network && trimmed.starts_with("id_str=") {
            block_has_id_str = true;
        } else if in_network && trimmed == "}" {
            if !block_has_id_str {
                out.push(format!("  id_str=\"{id}\""));
            }
            in_network = false;
        }
        out.push(line.to_string());
    }
    let mut result = out.join("\n");
    if conf.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Write a root-only (0600) file containing secrets (WiFi PSKs). The
/// mode is applied at create time so there's no world-readable window;
/// `set_permissions` afterwards covers the pre-existing-file case,
/// where the open-time mode doesn't apply.
fn write_secret_file(path: &str, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", path))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting permissions on {}", path))?;
    Ok(())
}

/// Select exactly one interface from NetworkManager's possibly multi-line output.
fn first_wifi_device(output: &str) -> Option<&str> {
    output.lines().map(str::trim).find(|name| {
        !name.is_empty() && *name != "ap0" && !name.chars().any(char::is_whitespace)
    })
}

/// Find the primary WiFi client device from NetworkManager.
///
/// Prefers the device backing an *active* WiFi connection (the right
/// answer when several wifi interfaces exist), but falls back to any
/// managed wifi device — a Pi being set up over Ethernet with WiFi
/// configured-but-disconnected would otherwise fail AP setup entirely.
async fn find_wifi_device() -> Result<String> {
    for _ in 0..5 {
        let output = sentryusb_shell::run(
            "bash",
            &[
                "-c",
                "nmcli -t -f TYPE,DEVICE c show --active | grep 802-11-wireless | grep -v ':ap0$' | cut -c17-",
            ],
        ).await.unwrap_or_default();
        let wlan = first_wifi_device(&output).unwrap_or_default().to_string();
        if !wlan.is_empty() {
            return Ok(wlan);
        }
        // No active wifi connection — fall back to the device list.
        // `:wifi$` excludes wifi-p2p entries; ap0 is our own AP iface.
        let output = sentryusb_shell::run(
            "bash",
            &[
                "-c",
                "nmcli -t -f DEVICE,TYPE device status | grep ':wifi$' | grep -v '^ap0:' | cut -d: -f1 | head -n1",
            ],
        ).await.unwrap_or_default();
        let wlan = first_wifi_device(&output).unwrap_or_default().to_string();
        if !wlan.is_empty() {
            info!("No active WiFi connection; using wifi device {} from device list", wlan);
            return Ok(wlan);
        }
        info!("Waiting for WiFi interface...");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("Could not determine WiFi client device")
}

/// Install the NM dispatcher that activates the AP on Away Mode events.
async fn install_ap_dispatcher(wlan: &str) -> Result<()> {
    // Dispatcher only brings the AP up when the away-mode flag file exists.
    // During normal operation the AP stays off so wlan0 can scan freely.
    let script = format!(
        "#!/bin/bash\n\
         # Recreate ap0 virtual interface when the wifi client comes up,\n\
         # but ONLY if Away Mode is active (flag file exists).\n\
         # Created by SentryUSB configure-ap.\n\
         \n\
         IFACE=\"$1\"\n\
         ACTION=\"$2\"\n\
         \n\
         if [ \"$IFACE\" = \"{wlan}\" ] && [ \"$ACTION\" = \"up\" ]\n\
         then\n\
         \x20\x20if [ -f /mutable/sentryusb_away_mode.json ]; then\n\
         \x20\x20\x20\x20if ! iw dev ap0 info &> /dev/null; then\n\
         \x20\x20\x20\x20\x20\x20iw dev {wlan} interface add ap0 type __ap || true\n\
         \x20\x20\x20\x20fi\n\
         \x20\x20\x20\x20iw {wlan} set power_save off 2>/dev/null || true\n\
         \x20\x20\x20\x20iw ap0 set power_save off 2>/dev/null || true\n\
         \x20\x20\x20\x20nmcli con up SENTRYUSB_AP 2>/dev/null || true\n\
         \x20\x20fi\n\
         fi\n"
    );

    let dispatcher_dir = "/etc/NetworkManager/dispatcher.d";
    std::fs::create_dir_all(dispatcher_dir)?;
    let path = format!("{}/10-sentryusb-ap", dispatcher_dir);
    std::fs::write(&path, script)?;
    let _ = sentryusb_shell::run("chmod", &["755", &path]).await;
    Ok(())
}

fn append_unless_contains(path: &str, needle: &str, text: &str) -> Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current.contains(needle) {
        return Ok(());
    }
    let mut new = current;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(text);
    std::fs::write(path, new)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_one_wifi_device_when_both_adapters_are_active() {
        assert_eq!(first_wifi_device("wlan1\nwlan0\n"), Some("wlan1"));
        assert_eq!(first_wifi_device("wlan0\nwlan1\n"), Some("wlan0"));
    }

    #[test]
    fn skips_empty_and_ap_interfaces() {
        assert_eq!(first_wifi_device("\n ap0\n wlan0\n"), Some("wlan0"));
        assert_eq!(first_wifi_device("\nap0\n"), None);
        assert_eq!(first_wifi_device(""), None);
    }

    #[test]
    fn tags_single_network_block() {
        let conf = "ctrl_interface=DIR=/var/run/wpa_supplicant GROUP=netdev\n\
                    update_config=1\n\
                    \n\
                    network={\n\
                    \tssid=\"home\"\n\
                    \tpsk=\"hunter22\"\n\
                    }\n";
        let tagged = tag_network_blocks_with_id_str(conf, "AP1");
        assert!(tagged.contains("  id_str=\"AP1\"\n}"));
        assert_eq!(tagged.matches("id_str").count(), 1);
        // Header lines untouched.
        assert!(tagged.starts_with("ctrl_interface="));
    }

    #[test]
    fn tags_every_network_block_but_not_cred_blocks() {
        let conf = "network={\n\
                    \tssid=\"home\"\n\
                    }\n\
                    cred={\n\
                    \tdomain=\"example.org\"\n\
                    }\n\
                    network={\n\
                    \tssid=\"work\"\n\
                    }\n";
        let tagged = tag_network_blocks_with_id_str(conf, "AP1");
        assert_eq!(tagged.matches("id_str=\"AP1\"").count(), 2);
        // The cred block's closing brace must NOT have been tagged: the
        // id_str line directly precedes only network-block braces.
        assert!(!tagged.contains("domain=\"example.org\"\n  id_str"));
    }

    #[test]
    fn skips_blocks_that_already_have_id_str() {
        let conf = "network={\n\
                    \tssid=\"home\"\n\
                    \tid_str=\"AP1\"\n\
                    }\n";
        let tagged = tag_network_blocks_with_id_str(conf, "AP1");
        assert_eq!(tagged.matches("id_str").count(), 1, "idempotent");
    }
}

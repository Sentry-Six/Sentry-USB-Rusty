//! Timed and geofence-controlled Wi-Fi access-point management.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{info, warn};
use sentryusb_setup::network::ap_profile::{self, ApProfile, AP_KEYFILE, AP_PROFILE};
use sentryusb_setup::network::networkd;

use crate::router::AppState;

const FLAG_FILE: &str = "/mutable/sentryusb_away_mode.json";
const HOSTAPD_CONF: &str = "/etc/hostapd/hostapd.conf";
const IFUPDOWN_AP_FILE: &str = "/etc/network/interfaces.d/sentryusb-ap";
/// Default to 5 GHz channel 36 when the station is not associated.
const DEFAULT_AP_CHANNEL: u32 = 36;
const DEFAULT_AP_HW_MODE: &str = "a";

/// Detects whether Wi-Fi is managed by NetworkManager or ifupdown.
fn use_networkmanager() -> bool {
    std::process::Command::new("systemctl")
        .args(["--quiet", "is-active", "NetworkManager.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Waits through NetworkManager's early-boot activation before choosing a backend.
async fn wait_for_networkmanager() -> bool {
    // Do not delay genuine ifupdown systems.
    let nm_managed = std::process::Command::new("systemctl")
        .args(["--quiet", "is-enabled", "NetworkManager.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !nm_managed {
        return false;
    }
    // Allow NetworkManager up to 30 seconds to activate.
    for _ in 0..60 {
        if use_networkmanager() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    warn!("[away-mode] NetworkManager enabled but not active within timeout; using NM path anyway");
    true
}

/// Returns the station channel and band. Shared-radio chipsets require the AP
/// to use the same channel; an unassociated station returns `None`.
async fn detect_sta_channel() -> Option<(u32, &'static str)> {
    let out = sentryusb_shell::run("iw", &["wlan0", "info"]).await.ok()?;
    for line in out.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("channel ") else { continue };
        let mut parts = rest.split_whitespace();
        let ch: u32 = parts.next()?.parse().ok()?;
        let freq_token = parts.next()?.trim_start_matches('(');
        let freq: u32 = freq_token.parse().ok()?;
        let hw_mode = if freq >= 5000 { "a" } else { "g" };
        return Some((ch, hw_mode));
    }
    None
}

/// Updates hostapd to match the station channel before bringing up `ap0`.
fn update_hostapd_channel(channel: u32, hw_mode: &str) -> Result<(), String> {
    // The helper is safe when root is already writable.
    let _ = std::process::Command::new("/root/bin/remountfs_rw").status();
    let content = std::fs::read_to_string(HOSTAPD_CONF)
        .map_err(|e| format!("read {}: {}", HOSTAPD_CONF, e))?;
    let mut new = String::new();
    let mut saw_channel = false;
    let mut saw_mode = false;
    for line in content.lines() {
        if line.starts_with("channel=") {
            new.push_str(&format!("channel={}", channel));
            saw_channel = true;
        } else if line.starts_with("hw_mode=") {
            new.push_str(&format!("hw_mode={}", hw_mode));
            saw_mode = true;
        } else {
            new.push_str(line);
        }
        new.push('\n');
    }
    if !saw_channel {
        new.push_str(&format!("channel={}\n", channel));
    }
    if !saw_mode {
        new.push_str(&format!("hw_mode={}\n", hw_mode));
    }
    std::fs::write(HOSTAPD_CONF, new)
        .map_err(|e| format!("write {}: {}", HOSTAPD_CONF, e))?;
    Ok(())
}
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_DURATION_MIN: u64 = 24 * 60;

/// Default home geofence radius (meters) when `AWAY_MODE_HOME_RADIUS_M`
/// is unset. Matches keep-accessory's default.
const DEFAULT_HOME_RADIUS_M: f64 = 120.0;
/// Stale GPS data holds the current AP state.
const STALE_FIX_SEC: i64 = 900;
/// Consecutive readings required before a home/away transition.
const CONFIRM_TICKS: u8 = 2;

#[derive(Serialize, Deserialize, Default, Clone)]
struct FlagData {
    #[serde(default)]
    expires_at: String,
    #[serde(default)]
    enabled_at: String,
    #[serde(default)]
    remaining_sec: i64,
    #[serde(default)]
    has_rtc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApPhase { Off, Starting, On }

struct Inner {
    /// `manual` uses the timer; `auto` follows the geofence.
    mode: &'static str, // "manual" | "auto"
    state: &'static str, // "idle" | "active" (manual timer only)
    has_rtc: bool,
    expires_at: Option<SystemTime>,
    enabled_at: Option<SystemTime>,
    /// Last committed geofence decision; `None` means undecided.
    last_is_home: Option<bool>,
    /// Candidate awaiting confirmation.
    pending_is_home: Option<bool>,
    /// Consecutive candidate readings.
    pending_count: u8,
    stop: std::sync::Arc<Notify>,
    /// Watcher generation; prevents a missed notification from leaving duplicates.
    epoch: u64,
    ap_phase: ApPhase,
    ap_generation: u64,
    ap_start_error: Option<String>,
    retry_at: Option<Instant>,
}

fn mgr() -> &'static Mutex<Inner> {
    static M: OnceLock<Mutex<Inner>> = OnceLock::new();
    M.get_or_init(|| {
        let has_rtc = std::path::Path::new("/dev/rtc0").exists();
        if has_rtc {
            info!("[away-mode] RTC detected — using timestamp-based expiration");
        } else {
            info!("[away-mode] No RTC — using countdown-based expiration");
        }
        Mutex::new(Inner {
            mode: "manual",
            state: "idle",
            has_rtc,
            expires_at: None,
            enabled_at: None,
            last_is_home: None,
            pending_is_home: None,
            pending_count: 0,
            stop: std::sync::Arc::new(Notify::new()),
            epoch: 0,
            ap_phase: ApPhase::Off,
            ap_generation: 0,
            ap_start_error: None,
            retry_at: None,
        })
    })
}

fn to_rfc3339(t: SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn from_rfc3339(s: &str) -> Option<SystemTime> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc).into())
}

fn remaining_seconds(expires: SystemTime) -> i64 {
    expires
        .duration_since(SystemTime::now())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn write_flag_file(inner: &Inner) {
    let expires = match inner.expires_at {
        Some(e) => e,
        None => return,
    };
    let enabled = inner.enabled_at.unwrap_or(SystemTime::now());
    let flag = FlagData {
        expires_at: to_rfc3339(expires),
        enabled_at: to_rfc3339(enabled),
        remaining_sec: remaining_seconds(expires),
        has_rtc: inner.has_rtc,
    };
    if let Ok(data) = serde_json::to_vec_pretty(&flag) {
        let tmp = format!("{}.tmp", FLAG_FILE);
        if std::fs::write(&tmp, &data).is_ok() {
            let _ = std::fs::rename(&tmp, FLAG_FILE);
        }
    }
}

fn remove_flag_file() {
    let _ = std::fs::remove_file(FLAG_FILE);
    let _ = std::fs::remove_file(format!("{}.tmp", FLAG_FILE));
}

fn away_mode_log(msg: &str) {
    use std::io::Write;
    const LOG_PATH: &str = "/mutable/archiveloop.log";
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(LOG_PATH)
    {
        let ts = chrono::Local::now().format("%a %e %b %H:%M:%S %Z %Y");
        let _ = writeln!(f, "{}: [away-mode] {}", ts, msg);
    }
}

// ─────────────────────────────────────────────────────────────────────
// Automatic geofence mode owns AP state; telemetry supplies GPS fixes.
// ─────────────────────────────────────────────────────────────────────

/// Great-circle distance in meters.
fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_R_M: f64 = 6_371_000.0;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dphi = (lat2 - lat1).to_radians();
    let dlambda = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlambda / 2.0).sin().powi(2);
    // Guard the inverse-trigonometric domain against floating-point drift.
    2.0 * EARTH_R_M * a.sqrt().min(1.0).asin()
}

/// Maps distance into home/away/hold with a bounded hysteresis band.
fn band_is_home(dist_m: f64, radius_m: f64) -> Option<bool> {
    let margin = (0.15 * radius_m).clamp(15.0, 60.0);
    if dist_m < radius_m - margin {
        Some(true)
    } else if dist_m > radius_m + margin {
        Some(false)
    } else {
        None
    }
}

/// Confirms a banded reading across consecutive ticks before committing a flip.
fn fold_geofence(
    candidate: Option<bool>,
    last: Option<bool>,
    pending: &mut Option<bool>,
    pending_count: &mut u8,
) -> Option<bool> {
    let Some(cand) = candidate else {
        // A signal gap cancels partial confirmation.
        *pending = None;
        *pending_count = 0;
        return None;
    };
    if Some(cand) == last {
        // A confirmed-state reading cancels any pending flip.
        *pending = None;
        *pending_count = 0;
        return None;
    }
    if *pending == Some(cand) {
        *pending_count = pending_count.saturating_add(1);
    } else {
        *pending = Some(cand);
        *pending_count = 1;
    }
    if *pending_count >= CONFIRM_TICKS {
        *pending = None;
        *pending_count = 0;
        Some(cand)
    } else {
        None
    }
}

/// Seeds the decision from the flag: present means away, absent means home.
fn auto_seed_decision(flag_exists: bool) -> Option<bool> {
    Some(!flag_exists)
}

/// An away flag requires explicitly starting the AP during boot restore.
fn should_start_ap_on_boot(flag_exists: bool) -> bool {
    auto_seed_decision(flag_exists) == Some(false)
}

/// Returns the last GPS fix and its nonnegative age.
fn read_gps_fix() -> Option<(f64, f64, i64)> {
    let s = std::fs::read_to_string(crate::keep_accessory::gps_file()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let lat = v.get("lat").and_then(|x| x.as_f64())?;
    let lon = v.get("lon").and_then(|x| x.as_f64())?;
    let ts = v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((lat, lon, (now - ts).max(0)))
}

/// Reads the shared home center and Away Mode-specific radius.
fn read_away_geofence() -> (Option<f64>, Option<f64>, f64) {
    let config_path = sentryusb_config::find_config_path();
    match sentryusb_config::parse_file(config_path) {
        Ok((active, commented)) => {
            let g = |k: &str| sentryusb_config::get_config_value(&active, &commented, k);
            let lat = g("KEEP_ACCESSORY_HOME_LAT").and_then(|s| s.trim().parse::<f64>().ok());
            // Normalize legacy world-copy longitudes.
            let lon = g("KEEP_ACCESSORY_HOME_LON")
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(crate::normalize_lon);
            let radius = g("AWAY_MODE_HOME_RADIUS_M")
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|r| *r > 0.0)
                .unwrap_or(DEFAULT_HOME_RADIUS_M);
            (lat, lon, radius)
        }
        Err(_) => (None, None, DEFAULT_HOME_RADIUS_M),
    }
}

/// Reads the config-backed automatic-mode flag shared with telemetry.
fn read_auto_enabled_config() -> bool {
    let config_path = sentryusb_config::find_config_path();
    if let Ok((active, commented)) = sentryusb_config::parse_file(config_path) {
        if let Some(v) = sentryusb_config::get_config_value(&active, &commented, "AWAY_MODE_AUTO_ENABLED") {
            return matches!(v.trim(), "yes" | "true" | "1");
        }
    }
    false
}

/// Persists automatic-mode AP state for the NetworkManager dispatcher.
fn write_auto_flag_file() {
    let body = serde_json::json!({
        "mode": "auto",
        "enabled_at": to_rfc3339(SystemTime::now()),
    });
    if let Ok(data) = serde_json::to_vec_pretty(&body) {
        let tmp = format!("{}.tmp", FLAG_FILE);
        if std::fs::write(&tmp, &data).is_ok() {
            let _ = std::fs::rename(&tmp, FLAG_FILE);
        }
    }
}

/// Evaluates one geofence tick and toggles the AP only after a confirmed flip.
async fn auto_eval_tick(my_epoch: u64) {
    let fix = read_gps_fix();
    let (home_lat, home_lon, radius_m) = read_away_geofence();

    // Missing/stale GPS or an unset home geofence holds state.
    let candidate = match (fix, home_lat, home_lon) {
        (Some((la, lo, age)), Some(hla), Some(hlo)) if age <= STALE_FIX_SEC => {
            band_is_home(distance_m(la, lo, hla, hlo), radius_m)
        }
        _ => None,
    };

    // Commit and schedule the toggle under one lock to serialize mode changes.
    let mut inner = mgr().lock().unwrap();
    if inner.mode != "auto" || inner.epoch != my_epoch {
        return; // mode changed or watcher superseded — don't touch the AP
    }
    let last = inner.last_is_home;
    let mut pending = inner.pending_is_home;
    let mut count = inner.pending_count;
    let decision = fold_geofence(candidate, last, &mut pending, &mut count);
    inner.pending_is_home = pending;
    inner.pending_count = count;
    if let Some(is_home) = decision {
        inner.last_is_home = Some(is_home);
        if is_home {
            // Remove the flag before stopping the AP to prevent resurrection.
            remove_flag_file();
            request_ap_stop(&mut inner);
            away_mode_log("Automatic: at home — stopping AP (rejoining home WiFi)");
            info!("[away-mode] auto: home → AP off");
        } else {
            // Persist away state before starting the AP.
            write_auto_flag_file();
            request_ap_start(&mut inner);
            away_mode_log("Automatic: away from home — starting AP");
            info!("[away-mode] auto: away → AP on");
        }
    } else if candidate == Some(false) && inner.last_is_home == Some(false)
        && inner.ap_phase == ApPhase::Off
        && inner.retry_at.is_some_and(|at| Instant::now() >= at)
    {
        // Retry only with a fresh, still-away fix, and back off after failures.
        write_auto_flag_file();
        request_ap_start(&mut inner);
    }
}

/// Polls the geofence until mode or watcher generation changes.
fn spawn_auto_watcher(stop: std::sync::Arc<Notify>, my_epoch: u64) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.notified() => return,
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            {
                let inner = mgr().lock().unwrap();
                if inner.mode != "auto" || inner.epoch != my_epoch {
                    return;
                }
            }
            check_running_ap().await;
            auto_eval_tick(my_epoch).await;
        }
    });
}

/// Finds the active or first managed Wi-Fi client device, excluding `ap0`.
async fn find_wlan_device() -> Option<String> {
    for cmd in [
        "nmcli -t -f TYPE,DEVICE c show --active | grep 802-11-wireless | grep -v ':ap0$' | cut -c17- | head -n1",
        "nmcli -t -f DEVICE,TYPE device status | grep ':wifi$' | grep -v '^ap0:' | cut -d: -f1 | head -n1",
    ] {
        if let Ok(out) = sentryusb_shell::run("bash", &["-c", cmd]).await {
            let dev = out.trim().to_string();
            if !dev.is_empty() {
                return Some(dev);
            }
        }
    }
    None
}

/// Creates the `ap0` virtual interface when absent.
async fn ensure_ap0() -> Result<(), String> {
    if sentryusb_shell::run("iw", &["dev", "ap0", "info"]).await.is_ok() {
        return Ok(());
    }
    let wlan = find_wlan_device()
        .await
        .ok_or_else(|| "no wifi client device found".to_string())?;
    sentryusb_shell::run("iw", &["dev", &wlan, "interface", "add", "ap0", "type", "__ap"])
        .await
        .map_err(|e| format!("creating ap0 on {}: {}", wlan, e))?;
    // Disable power saving on both sides of the shared radio.
    let _ = sentryusb_shell::run("iw", &[&wlan, "set", "power_save", "off"]).await;
    let _ = sentryusb_shell::run("iw", &["ap0", "set", "power_save", "off"]).await;
    Ok(())
}

/// Waits for NetworkManager to make the newly created `ap0` usable.
async fn wait_for_ap0_ready() -> bool {
    for _ in 0..20 {
        if let Ok(out) =
            sentryusb_shell::run("nmcli", &["-t", "-f", "DEVICE,STATE", "device", "status"]).await
        {
            if let Some(state) = out.lines().find_map(|l| l.strip_prefix("ap0:")) {
                let state = state.trim();
                if state.starts_with("disconnected") || state.starts_with("connected") {
                    return true;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn start_ap() -> Result<(), String> {
        if networkd::applicable().await.map_err(|error| format!("{error:#}"))? {
            return networkd::start().await.map_err(|error| format!("{error:#}"));
        }
        if wait_for_networkmanager().await {
            ensure_ap0().await?;
            // Activation races NetworkManager unless `ap0` is ready.
            if !wait_for_ap0_ready().await {
                away_mode_log(
                    "ap0 did not reach a ready state within timeout; attempting AP activation anyway",
                );
                warn!("[away-mode] ap0 not ready within timeout; activating anyway");
            }
            match sentryusb_shell::run("nmcli", &["con", "up", AP_PROFILE]).await {
                Ok(_) => {
                    away_mode_log("AP started");
                    info!("[away-mode] AP started");
                }
                Err(e) => {
                    return Err(format!("Failed to bring up AP: {e}"));
                }
            }
        } else {
            // Shared-radio chipsets require AP and station to share a channel.
            let (channel, hw_mode) = detect_sta_channel()
                .await
                .unwrap_or((DEFAULT_AP_CHANNEL, DEFAULT_AP_HW_MODE));
            update_hostapd_channel(channel, hw_mode)?;
            match sentryusb_shell::run("ifup", &["ap0"]).await {
                Ok(_) => {
                    away_mode_log(&format!(
                        "AP started (ifupdown, channel {} {})",
                        channel, hw_mode
                    ));
                    info!(
                        "[away-mode] AP started via ifupdown channel={} hw_mode={}",
                        channel, hw_mode
                    );
                }
                Err(e) => {
                    return Err(format!("ifup ap0 failed: {e}"));
                }
            }
        }
    Ok(())
}

async fn stop_ap() -> Result<(), String> {
        if networkd::applicable().await.map_err(|error| format!("{error:#}"))? {
            return networkd::stop().await.map_err(|error| format!("{error:#}"));
        }
        if use_networkmanager() {
            match sentryusb_shell::run("nmcli", &["con", "down", AP_PROFILE]).await {
                Ok(_) => {
                    away_mode_log("AP stopped");
                    info!("[away-mode] AP stopped");
                }
                Err(e) => {
                    // Already-down profiles also return an error.
                    away_mode_log(&format!("nmcli con down failed (AP may already be down): {}", e));
                    warn!("[away-mode] nmcli con down failed: {}", e);
                }
            }
        } else {
            // Stop daemons explicitly in case ifupdown hooks are missing.
            match sentryusb_shell::run("ifdown", &["ap0"]).await {
                Ok(_) => {
                    away_mode_log("AP stopped (ifupdown)");
                    info!("[away-mode] AP stopped via ifupdown");
                }
                Err(e) => {
                    away_mode_log(&format!("ifdown ap0 failed (AP may already be down): {}", e));
                    warn!("[away-mode] ifdown ap0 failed: {}", e);
                }
            }
            let _ = sentryusb_shell::run("systemctl", &["stop", "hostapd"]).await;
            let _ = sentryusb_shell::run("systemctl", &["stop", "dnsmasq"]).await;
        }
        // Remove `ap0` so the shared radio can scan normally.
        let _ = sentryusb_shell::run("iw", &["dev", "ap0", "del"]).await;
    Ok(())
}

fn ap_operations() -> &'static tokio::sync::Mutex<()> {
    static OPERATIONS: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    OPERATIONS.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn request_ap_start(inner: &mut Inner) {
    inner.ap_generation += 1;
    inner.ap_phase = ApPhase::Starting;
    inner.ap_start_error = None;
    inner.retry_at = None;
    let generation = inner.ap_generation;
    tokio::spawn(async move {
        // Serialize radio changes. An obsolete start must finish cleanup before
        // a newer start/stop can take ownership of the interface.
        let _operation = ap_operations().lock().await;
        if mgr().lock().unwrap().ap_generation != generation { return; }
        let mut result = start_ap().await;
        let current = mgr().lock().unwrap().ap_generation == generation;
        if result.is_err() || !current {
            if let Err(error) = stop_ap().await {
                away_mode_log(&format!("AP cleanup failed: {error}"));
                result = Err(format!("{}; WiFi recovery failed: {error}", result.err().unwrap_or_else(|| "Hotspot startup was canceled".into())));
            }
        }
        let mut inner = mgr().lock().unwrap();
        complete_ap_start(&mut inner, generation, result);
    });
}

fn complete_ap_start(inner: &mut Inner, generation: u64, result: Result<(), String>) {
    if finish_ap_start(inner, generation, result) {
        // Failure flags must not suppress archiveloop WiFi recovery or
        // make a dispatcher resurrect a hotspot that never started.
        remove_flag_file();
        away_mode_log(&format!("Hotspot failed: {}", inner.ap_start_error.as_deref().unwrap_or_default()));
    }
}

/// Only the new backend is health-checked here; existing backends keep their
/// current monitoring. A service exit after startup must also release WiFi.
async fn check_running_ap() {
    let generation = {
        let inner = mgr().lock().unwrap();
        if inner.ap_phase != ApPhase::On { return; }
        inner.ap_generation
    };
    if !networkd::applicable().await.unwrap_or(false) { return; }
    let _operation = ap_operations().lock().await;
    if mgr().lock().unwrap().ap_generation != generation { return; }
    if let Err(error) = networkd::check().await {
        let mut message = format!("{error:#}");
        if let Err(cleanup) = networkd::stop().await {
            message.push_str(&format!("; WiFi recovery failed: {cleanup:#}"));
        }
        complete_ap_start(&mut mgr().lock().unwrap(), generation, Err(message));
    }
}

/// Returns true only when the current request failed and owns flag cleanup.
fn finish_ap_start(inner: &mut Inner, generation: u64, result: Result<(), String>) -> bool {
    if inner.ap_generation != generation { return false; }
    match result {
        Ok(()) => { inner.ap_phase = ApPhase::On; false }
        Err(error) => {
            inner.ap_phase = ApPhase::Off;
            inner.ap_start_error = Some(error);
            inner.retry_at = Some(Instant::now() + Duration::from_secs(60));
            if inner.mode == "manual" {
                inner.state = "idle";
                inner.enabled_at = None;
                inner.expires_at = None;
                inner.stop.notify_waiters();
                inner.epoch += 1;
            }
            true
        }
    }
}

fn request_ap_stop(inner: &mut Inner) {
    inner.ap_generation += 1;
    inner.ap_phase = ApPhase::Off;
    inner.ap_start_error = None;
    inner.retry_at = None;
    let generation = inner.ap_generation;
    tokio::spawn(async move {
        let _operation = ap_operations().lock().await;
        if mgr().lock().unwrap().ap_generation != generation { return; }
        if let Err(error) = stop_ap().await {
            away_mode_log(&format!("Hotspot cleanup failed: {error}"));
            let mut inner = mgr().lock().unwrap();
            if inner.ap_generation == generation { inner.ap_start_error = Some(error); }
        }
    });
}

async fn get_ap_info() -> anyhow::Result<Option<ApProfile>> {
    let keyfile_exists = std::path::Path::new(AP_KEYFILE).try_exists()
        .context("Could not check the WiFi Access Point configuration file")?;
    // A stopped/starting NM service is a lookup failure, not an ifupdown
    // installation with no AP. Keep using NM when it owns the saved profile.
    let nm_enabled = std::process::Command::new("systemctl")
        .args(["--quiet", "is-enabled", "NetworkManager.service"])
        .status().map(|status| status.success()).unwrap_or(false);
    let nm_active = use_networkmanager();
    let using_networkd = !nm_active && !nm_enabled && networkd::applicable().await?;
    let using_nm = !using_networkd && (nm_active || nm_enabled || keyfile_exists);
    let profile = if using_nm {
        ap_profile::read_nm_ap_profile().await?
    } else if using_networkd {
        networkd::profile().await?
    } else {
        read_hostapd_profile(HOSTAPD_CONF, IFUPDOWN_AP_FILE, "/etc/network/interfaces")?
    };
    if profile.is_none() {
        let missing = if using_nm { "NetworkManager has no SENTRYUSB_AP profile" }
            else if using_networkd { "the Armbian Netplan/networkd hotspot profile is missing" }
            else { "the hostapd AP configuration is missing" };
        explain_missing_ap_profile(sentryusb_config::find_config_path(), keyfile_exists && using_nm, missing)?;
    }
    Ok(profile)
}

fn explain_missing_ap_profile(config_path: &str, keyfile_exists: bool, missing: &str) -> anyhow::Result<()> {
    if keyfile_exists {
        bail!("The WiFi Access Point configuration file exists, but NetworkManager has not loaded SENTRYUSB_AP. Check the setup log for the load error.");
    }
    if std::path::Path::new(config_path).try_exists().context("Could not check the saved AP settings")? {
        let (config, _) = sentryusb_config::parse_file(config_path)
            .context("Could not read the saved AP settings")?;
        if config.get("AP_SSID").is_some_and(|ssid| !ssid.is_empty()) {
            bail!("WiFi Access Point settings are saved, but {missing}. AP setup is incomplete; check the setup log and retry Apply & Run Setup.");
        }
    }
    Ok(())
}

fn read_optional_ap_file(path: &str) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not read AP configuration {path}")),
    }
}

fn read_hostapd_profile(hostapd: &str, ap_interfaces: &str, interfaces: &str) -> anyhow::Result<Option<ApProfile>> {
    let hostapd_config = read_optional_ap_file(hostapd)?;
    let dedicated = read_optional_ap_file(ap_interfaces)?;
    if hostapd_config.is_none() && dedicated.is_none() {
        return Ok(None);
    }
    let hostapd_config = hostapd_config.with_context(|| format!("The AP interface is configured, but {hostapd} is missing"))?;
    let ssid = hostapd_config.lines().find_map(|line| line.trim().strip_prefix("ssid="))
        .filter(|ssid| !ssid.is_empty()).context("The hostapd AP configuration has no network name (SSID)")?;
    // Older setup versions (including the Rust hostapd installer) write
    // ap0 into the main interfaces file instead of the dedicated snippet.
    let network = match dedicated {
        Some(network) => network,
        None => read_optional_ap_file(interfaces)?.context("The AP configuration has no network interfaces file")?,
    };
    let mut in_ap = false;
    for line in network.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&"iface") {
            in_ap = fields.get(1) == Some(&"ap0") && fields.get(2) == Some(&"inet");
        } else if in_ap && fields.first() == Some(&"address") {
            let ip = fields.get(1).unwrap_or(&"").split('/').next().unwrap_or_default();
            if ip.parse::<std::net::Ipv4Addr>().is_err() {
                bail!("The ap0 interface has no valid IPv4 address");
            }
            return Ok(Some(ApProfile { ssid: ssid.to_string(), ip: ip.to_string() }));
        }
    }
    bail!("The hostapd AP configuration exists, but the ap0 interface has no IPv4 address");
}

fn add_ap_status(snap: &mut serde_json::Value, profile: anyhow::Result<Option<ApProfile>>) {
    snap["ap_configured"] = serde_json::json!(matches!(profile, Ok(Some(_))));
    match profile {
        Ok(Some(profile)) => {
            snap["ap_ssid"] = serde_json::json!(profile.ssid);
            snap["ap_ip"] = serde_json::json!(profile.ip);
        }
        Ok(None) => {}
        Err(error) => snap["ap_error"] = serde_json::json!(format!("{error:#}")),
    }
}

fn require_ap_profile(profile: anyhow::Result<Option<ApProfile>>) -> Result<ApProfile, (StatusCode, Json<serde_json::Value>)> {
    match profile {
        Ok(Some(profile)) => Ok(profile),
        Ok(None) => Err(crate::json_error(StatusCode::PRECONDITION_FAILED,
            "AP not configured. Run setup with AP settings first.")),
        Err(error) => Err(crate::json_error(StatusCode::SERVICE_UNAVAILABLE, &format!("{error:#}"))),
    }
}

fn spawn_watcher(stop: std::sync::Arc<Notify>, my_epoch: u64) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.notified() => return,
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            check_running_ap().await;
            let expired = {
                let mut inner = mgr().lock().unwrap();
                if inner.state != "active" || inner.epoch != my_epoch {
                    return;
                }
                let expired = inner.expires_at.map_or(true, |e| SystemTime::now() >= e);
                if expired {
                    inner.state = "idle";
                    inner.expires_at = None;
                    inner.enabled_at = None;
                    remove_flag_file();
                    request_ap_stop(&mut inner);
                    true
                } else {
                    // Persist the countdown for systems without a reliable RTC.
                    write_flag_file(&inner);
                    false
                }
            };
            if expired {
                away_mode_log("Timer expired, disabling");
                info!("[away-mode] Timer expired");
                return;
            }
        }
    });
}

/// Restores automatic geofence state or a still-active manual timer at startup.
pub fn restore_from_file() {
    if networkd::has_runtime_owner() && !std::path::Path::new(FLAG_FILE).exists() {
        request_ap_stop(&mut mgr().lock().unwrap());
    }
    if read_auto_enabled_config() {
        let flag_exists = std::path::Path::new(FLAG_FILE).exists();
        let (notify, epoch) = {
            let mut inner = mgr().lock().unwrap();
            inner.mode = "auto";
            inner.state = "idle"; // auto doesn't use the timer state
            inner.expires_at = None;
            inner.enabled_at = None;
            // Match memory to the AP state restored by the dispatcher.
            inner.last_is_home = auto_seed_decision(flag_exists);
            inner.pending_is_home = None;
            inner.pending_count = 0;
            inner.stop = std::sync::Arc::new(Notify::new());
            inner.epoch += 1;
            if should_start_ap_on_boot(flag_exists) {
                request_ap_start(&mut inner);
            }
            (inner.stop.clone(), inner.epoch)
        };
        // Retain any persisted away decision.
        away_mode_log("Automatic Away Mode active on boot — geofence watcher started");
        info!("[away-mode] Automatic mode — geofence watcher started");
        // Away boots have no station-up event or geofence flip to start the AP.
        if should_start_ap_on_boot(flag_exists) {
            away_mode_log("Automatic: booted while away — starting AP");
            info!("[away-mode] auto: booted while away → starting AP");
        }
        spawn_auto_watcher(notify, epoch);
        return;
    }

    let data = match std::fs::read_to_string(FLAG_FILE) {
        Ok(d) => d,
        Err(_) => return,
    };
    let flag: FlagData = match serde_json::from_str(&data) {
        Ok(f) => f,
        Err(e) => {
            warn!("[away-mode] Invalid flag file, removing: {}", e);
            remove_flag_file();
            return;
        }
    };

    let has_rtc = { mgr().lock().unwrap().has_rtc };
    let remaining = if has_rtc {
        match from_rfc3339(&flag.expires_at) {
            Some(e) => e.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO),
            None => {
                remove_flag_file();
                return;
            }
        }
    } else {
        Duration::from_secs(flag.remaining_sec.max(0) as u64)
    };

    if remaining.is_zero() {
        info!("[away-mode] Flag file expired, cleaning up");
        remove_flag_file();
        request_ap_stop(&mut mgr().lock().unwrap());
        return;
    }

    let enabled_at = from_rfc3339(&flag.enabled_at).unwrap_or_else(SystemTime::now);
    let (notify, epoch) = {
        let mut inner = mgr().lock().unwrap();
        inner.state = "active";
        inner.enabled_at = Some(enabled_at);
        inner.expires_at = Some(SystemTime::now() + remaining);
        inner.stop = std::sync::Arc::new(Notify::new());
        inner.epoch += 1;
        request_ap_start(&mut inner);
        (inner.stop.clone(), inner.epoch)
    };

    away_mode_log(&format!(
        "Restored from flag file ({}s remaining, rtc: {})",
        remaining.as_secs(),
        has_rtc
    ));
    info!(
        "[away-mode] Restored from flag file ({}s remaining)",
        remaining.as_secs()
    );
    spawn_watcher(notify, epoch);
}

#[derive(Deserialize)]
pub struct EnableRequest {
    duration_min: Option<u64>,
}

/// POST /api/away-mode/enable
pub async fn enable(
    State(_s): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    let req: EnableRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return crate::json_error(StatusCode::BAD_REQUEST, "invalid request body"),
    };
    let minutes = req.duration_min.unwrap_or(0);
    if minutes == 0 {
        return crate::json_error(StatusCode::BAD_REQUEST, "duration_min must be positive");
    }
    if minutes > MAX_DURATION_MIN {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            "duration_min cannot exceed 24 hours (1440)",
        );
    }
    // Manual and automatic AP ownership are mutually exclusive.
    let in_auto = { mgr().lock().unwrap().mode == "auto" };
    if in_auto {
        return crate::json_error(
            StatusCode::CONFLICT,
            "Away Mode is in Automatic mode — switch to Manual to use the timer.",
        );
    }
    // Do not acknowledge activation without a configured AP.
    let profile = match require_ap_profile(get_ap_info().await) {
        Ok(profile) => profile,
        Err(response) => return response,
    };

    let duration = Duration::from_secs(minutes * 60);
    enum Action {
        Extended,
        Started(std::sync::Arc<Notify>, u64),
    }
    let (action, snap) = {
        let mut inner = mgr().lock().unwrap();
        if inner.state == "active" {
            inner.expires_at = Some(SystemTime::now() + duration);
            write_flag_file(&inner);
            away_mode_log(&format!("Extended (duration: {}m)", minutes));
            (Action::Extended, status_snapshot_sync(&inner))
        } else {
            inner.stop.notify_waiters();
            inner.state = "active";
            let now = SystemTime::now();
            inner.enabled_at = Some(now);
            inner.expires_at = Some(now + duration);
            inner.stop = std::sync::Arc::new(Notify::new());
            inner.epoch += 1;
            write_flag_file(&inner);
            away_mode_log(&format!(
                "Enabled (duration: {}m, rtc: {})",
                minutes, inner.has_rtc
            ));
            info!("[away-mode] Enabled (duration: {}m)", minutes);
            request_ap_start(&mut inner);
            (
                Action::Started(inner.stop.clone(), inner.epoch),
                status_snapshot_sync(&inner),
            )
        }
    };

    if let Action::Started(notify, epoch) = action {
        spawn_watcher(notify, epoch);
    }

    let mut snap = snap;
    add_ap_status(&mut snap, Ok(Some(profile)));
    (StatusCode::OK, Json(snap))
}

/// POST /api/away-mode/disable
pub async fn disable(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    {
        let mut inner = mgr().lock().unwrap();
        // Automatic mode must be exited through the mode endpoint.
        if inner.mode == "auto" {
            return crate::json_ok();
        }
        if inner.state == "idle" {
            return crate::json_ok();
        }
        inner.stop.notify_waiters();
        inner.state = "idle";
        inner.expires_at = None;
        inner.enabled_at = None;
        inner.epoch += 1;
        remove_flag_file();
        request_ap_stop(&mut inner);
    }
    away_mode_log("Disabled by user");
    info!("[away-mode] Disabled by user");
    crate::json_ok()
}

/// GET /api/away-mode/status
pub async fn status(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let (mut snap, mode) = {
        let inner = mgr().lock().unwrap();
        (status_snapshot_sync(&inner), inner.mode)
    };
    add_ap_status(&mut snap, get_ap_info().await);
    // Automatic mode also exposes its geofence and GPS freshness.
    if mode == "auto" {
        let (home_lat, home_lon, radius) = read_away_geofence();
        snap["geofence_configured"] =
            serde_json::Value::Bool(home_lat.is_some() && home_lon.is_some());
        snap["home_lat"] = serde_json::json!(home_lat);
        snap["home_lon"] = serde_json::json!(home_lon);
        snap["home_radius_m"] = serde_json::json!(radius);
        snap["ble_ready"] = serde_json::Value::Bool(crate::ble::is_ble_enabled());
        match read_gps_fix() {
            Some((_, _, age)) => {
                snap["gps_stale"] = serde_json::Value::Bool(age > STALE_FIX_SEC);
                snap["last_fix_age_sec"] = serde_json::json!(age);
            }
            None => {
                snap["gps_stale"] = serde_json::Value::Bool(true);
                snap["last_fix_age_sec"] = serde_json::Value::Null;
            }
        }
    }
    (StatusCode::OK, Json(snap))
}

#[derive(Deserialize)]
pub struct ModeBody {
    mode: String,
}

/// Persists and applies a `manual` or `auto` mode transition.
pub async fn set_mode(
    State(_s): State<AppState>,
    Json(body): Json<ModeBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    let want_auto = match body.mode.as_str() {
        "auto" => true,
        "manual" => false,
        other => {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                &format!("invalid mode {other:?} (expected \"manual\" or \"auto\")"),
            );
        }
    };

    if want_auto {
        if let Err(response) = require_ap_profile(get_ap_info().await) {
            return response;
        }
    }

    // Persist the mode before changing in-process ownership.
    let persist = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let config_path = sentryusb_config::find_config_path();
        let (mut active, _) = sentryusb_config::parse_file(config_path)?;
        active.insert(
            "AWAY_MODE_AUTO_ENABLED".to_string(),
            if want_auto { "yes" } else { "no" }.to_string(),
        );
        let _ = std::process::Command::new("bash")
            .args(["-c", "/root/bin/remountfs_rw"])
            .status();
        sentryusb_config::write_file(config_path, &active)?;
        Ok(())
    })
    .await;
    match persist {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("config write failed: {e}"),
            );
        }
        Err(e) => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("config task failed: {e}"),
            );
        }
    }

    if want_auto {
        let (notify, epoch) = {
            let mut inner = mgr().lock().unwrap();
            // Cancel the previous watcher generation.
            inner.stop.notify_waiters();
            inner.mode = "auto";
            inner.state = "idle";
            inner.expires_at = None;
            inner.enabled_at = None;
            inner.last_is_home = None;
            inner.pending_is_home = None;
            inner.pending_count = 0;
            inner.stop = std::sync::Arc::new(Notify::new());
            inner.epoch += 1;
            if inner.ap_phase == ApPhase::Starting {
                remove_flag_file();
                request_ap_stop(&mut inner);
            }
            (inner.stop.clone(), inner.epoch)
        };
        away_mode_log("Switched to Automatic (geofence) mode");
        info!("[away-mode] mode → auto");
        spawn_auto_watcher(notify, epoch);
    } else {
        {
            let mut inner = mgr().lock().unwrap();
            // Leave manual mode idle with no watcher.
            inner.stop.notify_waiters();
            inner.mode = "manual";
            inner.state = "idle";
            inner.expires_at = None;
            inner.enabled_at = None;
            inner.last_is_home = None;
            inner.pending_is_home = None;
            inner.pending_count = 0;
            inner.epoch += 1;
            remove_flag_file();
            request_ap_stop(&mut inner);
        }
        away_mode_log("Switched to Manual mode — AP stopped");
        info!("[away-mode] mode → manual");
    }

    let snap = {
        let inner = mgr().lock().unwrap();
        status_snapshot_sync(&inner)
    };
    (StatusCode::OK, Json(snap))
}

/// Returns the shared home center and Away Mode-specific radius.
pub async fn config_get(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let (home_lat, home_lon, home_radius_m) = read_away_geofence();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "home_lat": home_lat,
            "home_lon": home_lon,
            "home_radius_m": home_radius_m,
        })),
    )
}

#[derive(Deserialize)]
pub struct AwayConfigBody {
    pub home_lat: Option<f64>,
    pub home_lon: Option<f64>,
    pub home_radius_m: Option<f64>,
}

/// Persists the shared home center and Away Mode-specific radius.
pub async fn config_set(
    State(_s): State<AppState>,
    Json(body): Json<AwayConfigBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    // A center change invalidates charging's shared-home cache; radii are separate.
    let home_moving = body.home_lat.is_some() || body.home_lon.is_some();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let config_path = sentryusb_config::find_config_path();
        let (mut active, _) = sentryusb_config::parse_file(config_path)?;
        if let Some(lat) = body.home_lat.filter(|v| v.is_finite()) {
            let lat = lat.clamp(-90.0, 90.0);
            active.insert("KEEP_ACCESSORY_HOME_LAT".to_string(), format!("{lat:.6}"));
        }
        if let Some(lon) = body.home_lon.filter(|v| v.is_finite()) {
            // Normalize Leaflet world-copy longitudes.
            let lon = crate::normalize_lon(lon);
            active.insert("KEEP_ACCESSORY_HOME_LON".to_string(), format!("{lon:.6}"));
        }
        if let Some(r) = body.home_radius_m {
            // Match keep-accessory's 20 m–2 km bounds.
            let r = r.clamp(20.0, 2000.0).round() as i64;
            active.insert("AWAY_MODE_HOME_RADIUS_M".to_string(), r.to_string());
        }
        let _ = std::process::Command::new("bash")
            .args(["-c", "/root/bin/remountfs_rw"])
            .status();
        sentryusb_config::write_file(config_path, &active)?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            info!("[away-mode] geofence config updated");
            if home_moving {
                crate::charging::invalidate_charging_list();
            }
            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }
        Ok(Err(e)) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {e}"),
        ),
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config task failed: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starting_session(mode: &'static str) -> Inner {
        Inner {
            mode, state: if mode == "manual" { "active" } else { "idle" }, has_rtc: false,
            expires_at: Some(SystemTime::now() + Duration::from_secs(600)),
            enabled_at: Some(SystemTime::now()), last_is_home: Some(false),
            pending_is_home: None, pending_count: 0,
            stop: std::sync::Arc::new(Notify::new()), epoch: 1,
            ap_phase: ApPhase::Starting, ap_generation: 7, ap_start_error: None, retry_at: None,
        }
    }

    #[test]
    fn manual_start_is_not_active_until_success_and_failure_cancels_its_timer() {
        let mut inner = starting_session("manual");
        let pending = status_snapshot_sync(&inner);
        assert_eq!(pending["state"], "idle");
        assert_eq!(pending["ap_starting"], true);
        assert_eq!(pending["ap_on"], false);
        assert!(finish_ap_start(&mut inner, 7, Err("hostapd did not become ready".into())), "current failure must remove its flag");
        assert_eq!(inner.state, "idle");
        assert!(inner.expires_at.is_none() && inner.enabled_at.is_none());
        assert_eq!(inner.epoch, 2, "the timer watcher is invalidated");
        let failed = status_snapshot_sync(&inner);
        assert_eq!(failed["ap_starting"], false);
        assert_eq!(failed["ap_on"], false);
        assert_eq!(failed["ap_start_error"], "hostapd did not become ready");
    }

    #[test]
    fn automatic_failure_preserves_location_but_not_active_state_and_backs_off() {
        let mut inner = starting_session("auto");
        assert!(finish_ap_start(&mut inner, 7, Err("failed to create ap0".into())));
        let status = status_snapshot_sync(&inner);
        assert_eq!(status["is_home"], false);
        assert_eq!(status["ap_on"], false);
        assert_eq!(inner.epoch, 1, "automatic watching must continue");
        assert!(inner.retry_at.unwrap() > Instant::now());
    }

    #[test]
    fn stale_start_completion_cannot_reactivate_or_clear_a_new_sessions_flag() {
        let mut inner = starting_session("manual");
        inner.ap_generation = 8;
        inner.ap_phase = ApPhase::Off;
        inner.state = "idle";
        assert!(!finish_ap_start(&mut inner, 7, Ok(())));
        assert_eq!(inner.ap_phase, ApPhase::Off);
        inner.ap_phase = ApPhase::Starting;
        assert!(!finish_ap_start(&mut inner, 7, Err("obsolete failure".into())));
        assert_eq!(inner.ap_phase, ApPhase::Starting);
        assert!(inner.ap_start_error.is_none());
    }

    #[test]
    fn successful_start_confirms_active_state_for_both_modes() {
        for mode in ["manual", "auto"] {
            let mut inner = starting_session(mode);
            assert!(!finish_ap_start(&mut inner, 7, Ok(())));
            let status = status_snapshot_sync(&inner);
            assert_eq!(status["ap_on"], true);
            assert_eq!(status["ap_starting"], false);
            assert_eq!(status["state"], if mode == "manual" { "active" } else { "idle" });
        }
    }

    #[test]
    fn ap_status_distinguishes_missing_profile_from_detection_error() {
        let mut missing = serde_json::json!({ "state": "idle" });
        add_ap_status(&mut missing, Ok(None));
        assert_eq!(missing["ap_configured"], false);
        assert!(missing.get("ap_error").is_none());

        let mut failed = serde_json::json!({ "state": "active" });
        add_ap_status(&mut failed, Err(anyhow::anyhow!("NetworkManager is not running")));
        assert_eq!(failed["state"], "active");
        assert_eq!(failed["ap_configured"], false);
        assert_eq!(failed["ap_error"], "NetworkManager is not running");

        let mut ready = serde_json::json!({ "state": "idle" });
        add_ap_status(&mut ready, Ok(Some(ApProfile { ssid: "Road WiFi".into(), ip: "192.168.66.1".into() })));
        assert_eq!(ready["ap_configured"], true);
        assert_eq!(ready["ap_ssid"], "Road WiFi");
        assert!(ready.get("ap_error").is_none());
    }

    #[test]
    fn activation_returns_the_detection_error_instead_of_setup_instructions() {
        assert_eq!(require_ap_profile(Ok(None)).unwrap_err().0, StatusCode::PRECONDITION_FAILED);
        let (code, Json(body)) = require_ap_profile(Err(anyhow::anyhow!("AP profile lookup timed out"))).unwrap_err();
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "AP profile lookup timed out");
    }

    #[test]
    fn saved_ap_settings_and_unloaded_keyfile_are_reported_as_incomplete_setup() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("sentryusb.conf");
        let path = config.to_str().unwrap();
        let missing = "NetworkManager has no SENTRYUSB_AP profile";
        assert!(explain_missing_ap_profile(path, false, missing).is_ok());
        std::fs::write(&config, "# export AP_SSID='optional'\n").unwrap();
        assert!(explain_missing_ap_profile(path, false, missing).is_ok());
        std::fs::write(&config, "export AP_SSID='Road WiFi'\n").unwrap();
        let error = explain_missing_ap_profile(path, false, missing).unwrap_err().to_string();
        assert!(error.contains("settings are saved"));
        assert!(error.contains("no SENTRYUSB_AP profile"));
        let error = explain_missing_ap_profile(path, true, missing).unwrap_err().to_string();
        assert!(error.contains("has not loaded SENTRYUSB_AP"));
    }

    #[test]
    fn hostapd_detection_accepts_both_installer_layouts_and_rejects_partial_config() {
        let dir = tempfile::tempdir().unwrap();
        let hostapd = dir.path().join("hostapd.conf");
        let dedicated = dir.path().join("sentryusb-ap");
        let interfaces = dir.path().join("interfaces");
        let read = || read_hostapd_profile(hostapd.to_str().unwrap(), dedicated.to_str().unwrap(), interfaces.to_str().unwrap());
        assert_eq!(read().unwrap(), None);
        std::fs::write(&hostapd, "ssid=Road WiFi\nwpa_passphrase=never-return-this\n").unwrap();
        assert!(read().is_err(), "an SSID alone is not a complete profile");
        let stanza = "iface wlan0 inet static\n  address 10.0.0.2\niface ap0 inet static\n  address 192.168.66.1\n";
        std::fs::write(&interfaces, stanza).unwrap();
        assert_eq!(read().unwrap().unwrap().ip, "192.168.66.1");
        std::fs::write(&dedicated, stanza).unwrap();
        std::fs::remove_file(&interfaces).unwrap();
        assert_eq!(read().unwrap().unwrap().ssid, "Road WiFi");
        std::fs::remove_file(&hostapd).unwrap();
        assert!(read().unwrap_err().to_string().contains("is missing"));
        std::fs::create_dir(&hostapd).unwrap();
        let error = format!("{:#}", read().unwrap_err());
        assert!(error.contains("Could not read AP configuration"));
        assert!(!error.contains("never-return-this"));
    }

    #[test]
    fn distance_zero_for_same_point() {
        assert!(distance_m(37.5, -122.3, 37.5, -122.3) < 0.01);
    }

    #[test]
    fn distance_roughly_111km_per_degree_lat() {
        let d = distance_m(37.0, -122.0, 38.0, -122.0);
        assert!((d - 111_200.0).abs() < 1_000.0, "got {d}m");
    }

    #[test]
    fn band_inside_is_home_outside_is_away_middle_holds() {
        assert_eq!(band_is_home(50.0, 120.0), Some(true));
        assert_eq!(band_is_home(300.0, 120.0), Some(false));
        assert_eq!(band_is_home(120.0, 120.0), None);
        assert_eq!(band_is_home(110.0, 120.0), None);
    }

    #[test]
    fn fold_needs_two_ticks_to_commit() {
        let mut pending = None;
        let mut count = 0u8;
        assert_eq!(
            fold_geofence(Some(false), None, &mut pending, &mut count),
            None
        );
        assert_eq!(count, 1);
        assert_eq!(
            fold_geofence(Some(false), None, &mut pending, &mut count),
            Some(false)
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn fold_holds_on_no_fix_and_clears_pending() {
        let mut pending = Some(false);
        let mut count = 1u8;
        assert_eq!(fold_geofence(None, Some(true), &mut pending, &mut count), None);
        assert_eq!(pending, None);
        assert_eq!(count, 0);
    }

    #[test]
    fn fold_no_op_when_already_in_state() {
        let mut pending = None;
        let mut count = 0u8;
        assert_eq!(
            fold_geofence(Some(true), Some(true), &mut pending, &mut count),
            None
        );
    }

    #[test]
    fn fold_jitter_does_not_commit() {
        let mut pending = None;
        let mut count = 0u8;
        let last = Some(true);
        assert_eq!(fold_geofence(Some(false), last, &mut pending, &mut count), None);
        assert_eq!(fold_geofence(Some(true), last, &mut pending, &mut count), None);
        assert_eq!(fold_geofence(Some(false), last, &mut pending, &mut count), None);
        assert_eq!(count, 1);
    }

    #[test]
    fn distance_is_finite_for_extreme_points() {
        for (a, b, c, d) in [
            (0.0, 0.0, 0.0, 180.0),
            (90.0, 0.0, -90.0, 0.0),
            (0.0, 0.0, 0.0, 0.0),
        ] {
            assert!(
                distance_m(a, b, c, d).is_finite(),
                "distance {a},{b} -> {c},{d} must be finite"
            );
        }
    }

    #[test]
    fn auto_seed_decision_reflects_flag_file() {
        assert_eq!(auto_seed_decision(true), Some(false));
        assert_eq!(auto_seed_decision(false), Some(true));
    }

    #[test]
    fn boot_starts_ap_only_when_away() {
        assert!(should_start_ap_on_boot(true));
        assert!(!should_start_ap_on_boot(false));
    }
}

fn status_snapshot_sync(inner: &Inner) -> serde_json::Value {
    let mut v = serde_json::json!({
        "mode": inner.mode,
        "state": if inner.ap_phase == ApPhase::Starting { "idle" } else { inner.state },
        "has_rtc": inner.has_rtc,
        "ap_starting": inner.ap_phase == ApPhase::Starting,
        "ap_on": inner.ap_phase == ApPhase::On,
    });
    if let Some(error) = &inner.ap_start_error {
        v["ap_start_error"] = serde_json::json!(error);
    }
    if inner.mode == "auto" {
        // Location and actual hotspot state are separate: startup can fail.
        v["is_home"] = match inner.last_is_home {
            Some(h) => serde_json::Value::Bool(h),
            None => serde_json::Value::Null,
        };
    } else if inner.state == "active" {
        if let Some(exp) = inner.expires_at {
            v["expires_at"] = serde_json::Value::String(to_rfc3339(exp));
            v["remaining_sec"] =
                serde_json::Value::Number((remaining_seconds(exp).max(0)).into());
        }
        if let Some(en) = inner.enabled_at {
            v["enabled_at"] = serde_json::Value::String(to_rfc3339(en));
        }
    }
    v
}

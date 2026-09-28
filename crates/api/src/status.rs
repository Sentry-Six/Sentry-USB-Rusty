//! Status, storage, config, and WiFi API handlers.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use crate::router::AppState;

// Shared status snapshots keep UI polling independent of archive I/O.

#[derive(Clone, Default)]
struct CachedNetwork {
    wifi_ssid: String,
    /// Frequency in Hz, empty when unavailable.
    wifi_freq: String,
    wifi_ip: String,
    ether_ip: String,
    ether_speed: String,
    /// Cached device names; signal and throughput remain live.
    wifi_dev: String,
    eth_dev: String,
}

/// Reads `(link_quality/70, signal_dbm)` directly from `/proc/net/wireless`.
fn read_wireless_quality(dev: &str) -> Option<(String, Option<i32>)> {
    let data = std::fs::read_to_string("/proc/net/wireless").ok()?;
    for line in data.lines().skip(2) {
        let line = line.trim_start();
        let prefix = format!("{}:", dev);
        if !line.starts_with(&prefix) {
            continue;
        }
        let cols: Vec<&str> = line[prefix.len()..].split_whitespace().collect();
        if cols.len() < 3 {
            return None;
        }
        // Kernel fixed-point fields may end in a period.
        let link = cols[1].trim_end_matches('.').parse::<u32>().ok()?;
        let level = cols[2].trim_end_matches('.').parse::<i32>().ok();
        return Some((format!("{}/70", link), level));
    }
    None
}

struct StatusCache {
    network: Mutex<Option<(CachedNetwork, Instant)>>,
    network_refresh: tokio::sync::Mutex<()>,
}

static STATUS_CACHE: OnceLock<StatusCache> = OnceLock::new();

fn cache() -> &'static StatusCache {
    STATUS_CACHE.get_or_init(|| StatusCache {
        network: Mutex::new(None),
        network_refresh: tokio::sync::Mutex::new(()),
    })
}

const NETWORK_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Serialize)]
pub struct ManagedStorageHealth {
    pub state: &'static str,
    pub message: String,
    pub reserve_bytes: u64,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub cleanup_state: String,
    pub cleanup_sampled_at: Option<u64>,
}

impl ManagedStorageHealth {
    fn unknown() -> Self {
        Self { state: "unknown", message: "Storage status unavailable".into(), reserve_bytes: 0,
            free_bytes: 0, total_bytes: 0, cleanup_state: "unknown".into(), cleanup_sampled_at: None }
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub(crate) fn mount_writable(mounts: &str, path: &str) -> Option<bool> {
    mounts.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.get(1).copied() == Some(path)).then(|| fields.get(3)
            .is_some_and(|opts| opts.split(',').any(|o| o == "rw")))
    })
}

fn storage_verdict(mounted: Option<bool>, total: u64, free: u64, cleanup: &str, inode_stalled: bool) -> (&'static str, &'static str) {
    if mounted.is_none() { return ("fail", "Recording storage is not mounted"); }
    if mounted == Some(false) { return ("fail", "Recording storage is read-only"); }
    if total == 0 { return ("unknown", "Storage capacity unavailable"); }
    if free == 0 { return ("fail", "Recording storage is full"); }
    if inode_stalled { return ("fail", "Clip index cleanup needs attention"); }
    let reserve = (10 * 1024 * 1024 * 1024u64).saturating_add(total / 33);
    if free < reserve {
        return match cleanup {
            "failed" => ("warn", "Automatic cleanup could not restore recording headroom"),
            "unknown" => ("warn", "Recording headroom is low; cleanup status unavailable"),
            _ => ("recovering", "Automatic cleanup is restoring recording headroom"),
        };
    }
    ("healthy", "Storage managed automatically")
}

pub fn managed_storage_health() -> ManagedStorageHealth {
    static CACHE: OnceLock<Mutex<Option<(ManagedStorageHealth, Instant)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((health, at)) = &*cache {
        if at.elapsed() < Duration::from_secs(5) { return health.clone(); }
    }
    let health = read_managed_storage_health();
    *cache = Some((health.clone(), Instant::now()));
    health
}

fn read_managed_storage_health() -> ManagedStorageHealth {
    let mut health = ManagedStorageHealth::unknown();
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return health; };
    let mounted = mount_writable(&mounts, "/backingfiles");
    // Never mistake the root filesystem beneath a missing mount for recording storage.
    let (total, free) = if mounted.is_some() { statvfs_backing_files().unwrap_or_default() } else { (0, 0) };
    if let Some(value) = std::fs::read_to_string("/run/sentryusb_storage_cleanup.json").ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()) {
        health.cleanup_sampled_at = value["sampled_at"].as_u64();
        if health.cleanup_sampled_at.is_some_and(|at| unix_seconds().saturating_sub(at) < 120) {
            health.cleanup_state = value["state"].as_str().unwrap_or("unknown").to_string();
        }
    }
    let (state, message) = storage_verdict(mounted, total, free, &health.cleanup_state,
        std::path::Path::new("/run/sentryusb_inode_stall").exists());
    health.state = state;
    health.message = message.into();
    health.total_bytes = total;
    health.free_bytes = free;
    health.reserve_bytes = (10 * 1024 * 1024 * 1024u64).saturating_add(total / 33);
    health
}

/// Returns total and free bytes from `statvfs` without spawning `stat`.
fn statvfs_backing_files() -> Option<(u64, u64)> {
    let path = std::ffi::CString::new("/backingfiles/.").ok()?;
    // SAFETY: `statvfs` initializes this zeroed struct on a successful return.
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statvfs(path.as_ptr(), &mut buf) };
    if r != 0 {
        return None;
    }
    let frsize = buf.f_frsize as u64;
    let total = (buf.f_blocks as u64).saturating_mul(frsize);
    let free = (buf.f_bfree as u64).saturating_mul(frsize);
    Some((total, free))
}

#[derive(Clone)]
pub struct NetSample {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub taken_at: Instant,
    rates: Option<(u64, u64)>,
}

pub type NetSampler = Arc<Mutex<HashMap<String, NetSample>>>;


#[derive(Clone, Serialize)]
struct PiStatus {
    cpu_temp: String,
    num_snapshots: String,
    snapshot_oldest: String,
    snapshot_newest: String,
    total_space: String,
    free_space: String,
    uptime: String,
    drives_active: String,
    /// Host-link state; unlike `drives_active`, reflects actual enumeration.
    udc_state: String,
    /// Seconds since the last `cam_disk.bin` write, or -1 when unknown.
    cam_last_write_secs: i64,
    wifi_ssid: String,
    /// Wi-Fi frequency in Hz, empty when unavailable.
    wifi_freq: String,
    wifi_strength: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wifi_signal_dbm: Option<i32>,
    wifi_ip: String,
    ether_ip: String,
    ether_speed: String,
    sbc_model: String,
    fan_speed: String,
    wifi_rx_bps: u64,
    wifi_tx_bps: u64,
    ether_rx_bps: u64,
    ether_tx_bps: u64,
    wifi_rate_state: &'static str,
    ether_rate_state: &'static str,
    wifi_sample_age_ms: Option<u64>,
    ether_sample_age_ms: Option<u64>,
    sampled_at: u64,
    storage_health: ManagedStorageHealth,
    /// Final hostname segment used as the stable device suffix.
    device_suffix: String,
}

pub async fn get_status(
    State(state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Snapshot and backing-file metadata can block during archive I/O.
    static FS_CACHE: crate::ttl_cache::StaleWhileRevalidate<(), PiStatus> =
        crate::ttl_cache::StaleWhileRevalidate::new(Duration::from_secs(2));
    let mut s = match FS_CACHE.get((), || Some(status_fs_snapshot())).await {
        Some(s) => s,
        None => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "status temporarily unavailable",
            );
        }
    };

    // Cache stable network identity; keep signal and throughput live.
    let net = cached_network().await;
    s.wifi_ssid = net.wifi_ssid;
    s.wifi_freq = net.wifi_freq;
    s.wifi_ip = net.wifi_ip;
    s.ether_ip = net.ether_ip;
    s.ether_speed = net.ether_speed;
    if !net.wifi_dev.is_empty() {
        if let Some((strength, dbm)) = read_wireless_quality(&net.wifi_dev) {
            s.wifi_strength = strength;
            s.wifi_signal_dbm = dbm;
        }
        let (rx, tx, quality, age) = latest_throughput(&state.net_sampler, &net.wifi_dev);
        s.wifi_rx_bps = rx;
        s.wifi_tx_bps = tx;
        s.wifi_rate_state = quality;
        s.wifi_sample_age_ms = age;
    }
    if !net.eth_dev.is_empty() {
        let (rx, tx, quality, age) = latest_throughput(&state.net_sampler, &net.eth_dev);
        s.ether_rx_bps = rx;
        s.ether_tx_bps = tx;
        s.ether_rate_state = quality;
        s.ether_sample_age_ms = age;
    }

    (StatusCode::OK, Json(serde_json::to_value(s).unwrap_or_default()))
}

/// Collects direct filesystem status on the blocking pool.
fn status_fs_snapshot() -> PiStatus {
    let mut s = PiStatus {
        cpu_temp: String::new(),
        num_snapshots: "0".into(),
        snapshot_oldest: String::new(),
        snapshot_newest: String::new(),
        total_space: String::new(),
        free_space: String::new(),
        uptime: String::new(),
        drives_active: "no".into(),
        udc_state: String::new(),
        cam_last_write_secs: -1,
        wifi_ssid: String::new(),
        wifi_freq: String::new(),
        wifi_strength: String::new(),
        wifi_signal_dbm: None,
        wifi_ip: String::new(),
        ether_ip: String::new(),
        ether_speed: String::new(),
        sbc_model: String::new(),
        fan_speed: String::new(),
        wifi_rx_bps: 0,
        wifi_tx_bps: 0,
        ether_rx_bps: 0,
        ether_tx_bps: 0,
        wifi_rate_state: "disconnected",
        ether_rate_state: "disconnected",
        wifi_sample_age_ms: None,
        ether_sample_age_ms: None,
        sampled_at: unix_seconds(),
        storage_health: ManagedStorageHealth::unknown(),
        device_suffix: read_device_suffix(),
    };

    s.storage_health = managed_storage_health();
    s.total_space = s.storage_health.total_bytes.to_string();
    s.free_space = s.storage_health.free_bytes.to_string();
    s.sbc_model = get_sbc_model();

    if let Ok(data) = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp") {
        s.cpu_temp = data.trim().to_string();
    }

    s.fan_speed = read_fan_speed();

    if let Ok(data) = std::fs::read_to_string("/proc/uptime") {
        if let Some(secs) = data.split_whitespace().next() {
            s.uptime = secs.to_string();
        }
    }

    // Active requires both UDC binding and a configured backing file.
    if sentryusb_gadget::is_active() {
        s.drives_active = "yes".into();
    }
    s.udc_state = read_udc_state();
    s.cam_last_write_secs = cam_last_write_secs();

    // Slot numbers are not time-monotonic, so derive the range from mtimes.
    let scan = cached_snapshot_scan();
    s.num_snapshots = scan.count.to_string();
    if let Some(t) = scan.oldest_unix {
        s.snapshot_oldest = t.to_string();
    }
    if let Some(t) = scan.newest_unix {
        s.snapshot_newest = t.to_string();
    }

    s
}

/// Warm requests never wait for shell-derived identity refreshes.
async fn cached_network() -> CachedNetwork {
    let previous = cache().network.lock().unwrap().clone();
    if let Some((info, at)) = &previous {
        if at.elapsed() < NETWORK_TTL { return info.clone(); }
    }
    if let Some((info, _)) = previous {
        if let Ok(guard) = cache().network_refresh.try_lock() {
            tokio::spawn(async move {
                let _guard = guard;
                let fresh = compute_network_info().await;
                *cache().network.lock().unwrap() = Some((fresh, Instant::now()));
            });
        }
        return info;
    }
    let _guard = cache().network_refresh.lock().await;
    if let Some((info, _)) = &*cache().network.lock().unwrap() { return info.clone(); }
    let info = compute_network_info().await;
    *cache().network.lock().unwrap() = Some((info.clone(), Instant::now()));
    info
}

/// Fetches shell-derived Wi-Fi and Ethernet properties.
async fn compute_network_info() -> CachedNetwork {
    let mut info = CachedNetwork::default();

    // Skip potentially blocking Wi-Fi commands when the interface is down.
    let wifi_dev = find_net_device("wl*");
    if !wifi_dev.is_empty() && iface_is_up(&wifi_dev) {
        info.wifi_dev = wifi_dev.clone();
        let ssid_args = ["-r", wifi_dev.as_str()];
        let ip_args = ["-4", "addr", "show", wifi_dev.as_str()];
        // `iw` reports frequency in MHz.
        let iw_args = ["dev", wifi_dev.as_str(), "link"];
        let (ssid_r, ip_r, iw_r) = tokio::join!(
            sentryusb_shell::run_with_timeout(Duration::from_secs(2), "iwgetid", &ssid_args),
            sentryusb_shell::run_with_timeout(Duration::from_secs(2), "ip", &ip_args),
            sentryusb_shell::run_with_timeout(Duration::from_secs(2), "iw", &iw_args),
        );
        if let Ok(out) = ssid_r {
            info.wifi_ssid = out.trim().to_string();
        }
        if let Ok(out) = ip_r {
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("inet ") {
                    if let Some(addr) = trimmed.split_whitespace().nth(1) {
                        info.wifi_ip = addr.split('/').next().unwrap_or("").to_string();
                    }
                }
            }
        }
        if let Ok(out) = iw_r {
            for line in out.lines() {
                let trimmed = line.trim();
                // Convert MHz to the API's Hz-string format.
                if let Some(rest) = trimmed.strip_prefix("freq:") {
                    if let Ok(mhz) = rest.trim().parse::<u64>() {
                        info.wifi_freq = (mhz * 1_000_000).to_string();
                        break;
                    }
                }
            }
        }
    }

    // Apply the same blocking-command guard to Ethernet.
    let mut eth_dev = find_net_device("eth*");
    if eth_dev.is_empty() {
        eth_dev = find_net_device("en*");
    }
    if !eth_dev.is_empty() && iface_is_up(&eth_dev) {
        info.eth_dev = eth_dev.clone();
        let eth_ip_args = ["-4", "addr", "show", eth_dev.as_str()];
        let eth_tool_args = [eth_dev.as_str()];
        let (ip_r, ethtool_r) = tokio::join!(
            sentryusb_shell::run_with_timeout(Duration::from_secs(2), "ip", &eth_ip_args),
            sentryusb_shell::run_with_timeout(Duration::from_secs(2), "ethtool", &eth_tool_args),
        );
        if let Ok(out) = ip_r {
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("inet ") {
                    if let Some(addr) = trimmed.split_whitespace().nth(1) {
                        info.ether_ip = addr.split('/').next().unwrap_or("").to_string();
                    }
                }
            }
        }
        if let Ok(out) = ethtool_r {
            for line in out.lines() {
                if line.contains("Speed:") {
                    if let Some(val) = line.split(':').nth(1) {
                        info.ether_speed = val.trim().to_string();
                    }
                }
            }
        }
    }

    info
}


#[derive(Serialize)]
struct StorageBreakdown {
    cam_size: i64,
    music_size: i64,
    lightshow_size: i64,
    boombox_size: i64,
    snapshots_size: i64,
    total_space: i64,
    free_space: i64,
    storage_health: ManagedStorageHealth,
}

pub async fn get_storage_breakdown(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let (cam, music, lightshow, boombox) = tokio::join!(
        disk_usage("/backingfiles/cam_disk.bin"),
        disk_usage("/backingfiles/music_disk.bin"),
        disk_usage("/backingfiles/lightshow_disk.bin"),
        disk_usage("/backingfiles/boombox_disk.bin"),
    );
    let health = tokio::task::spawn_blocking(managed_storage_health).await.unwrap_or_else(|_| ManagedStorageHealth::unknown());
    let mut sb = StorageBreakdown {
        cam_size: cam,
        music_size: music,
        lightshow_size: lightshow,
        boombox_size: boombox,
        snapshots_size: 0,
        total_space: health.total_bytes as i64,
        free_space: health.free_bytes as i64,
        storage_health: health,
    };

    // Reflink clones make `du` unsuitable; derive snapshot usage by subtraction.
    let disk_images = sb.cam_size + sb.music_size + sb.lightshow_size + sb.boombox_size;
    let used = sb.total_space - sb.free_space;
    sb.snapshots_size = (used - disk_images).max(0);

    (StatusCode::OK, Json(serde_json::to_value(sb).unwrap_or_default()))
}


pub async fn get_config(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let has = |p: &str| -> String {
        if std::path::Path::new(p).exists() { "yes".into() } else { "no".into() }
    };

    // Surface BLE settings after explicit enablement or any existing BLE state.
    let uses_ble = if crate::ble::is_ble_enabled()
        || std::path::Path::new("/root/.ble/key_private.pem").exists()
        || std::path::Path::new("/root/.ble/paired").exists()
    {
        "yes".to_string()
    } else {
        "no".to_string()
    };

    (StatusCode::OK, Json(serde_json::json!({
        "has_cam": has("/backingfiles/cam_disk.bin"),
        "has_music": has("/backingfiles/music_disk.bin"),
        "has_lightshow": has("/backingfiles/lightshow_disk.bin"),
        "has_boombox": has("/backingfiles/boombox_disk.bin"),
        "uses_ble": uses_ble,
    })))
}


pub async fn get_wifi_config(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut ssid = String::new();
    let mut connected = false;
    let mut source = String::new();

    // Prefer the kernel association query; `nmcli` may trigger a scan.
    if let Ok(out) = sentryusb_shell::run("iwgetid", &["-r"]).await {
        let s = out.trim();
        if !s.is_empty() {
            ssid = s.to_string();
            connected = true;
            source = "iwgetid".into();
        }
    }

    // Fall back to NetworkManager.
    if ssid.is_empty() {
        if let Ok(out) = sentryusb_shell::run("nmcli", &["-t", "-f", "active,ssid", "dev", "wifi"]).await {
            for line in out.lines() {
                if line.starts_with("yes:") {
                    ssid = line.strip_prefix("yes:").unwrap_or("").to_string();
                    connected = true;
                    source = "networkmanager".into();
                    break;
                }
            }
        }
    }

    // Fall back to configured wpa_supplicant networks.
    if ssid.is_empty() {
        for p in &[
            "/etc/wpa_supplicant/wpa_supplicant.conf",
            "/boot/firmware/wpa_supplicant.conf",
            "/boot/wpa_supplicant.conf",
        ] {
            if let Ok(data) = std::fs::read_to_string(p) {
                for line in data.lines() {
                    let trimmed = line.trim();
                    if let Some(val) = trimmed.strip_prefix("ssid=") {
                        let val = val.trim_matches('"');
                        if !val.is_empty() {
                            ssid = val.to_string();
                            source = "wpa_supplicant".into();
                            break;
                        }
                    }
                }
                if !ssid.is_empty() {
                    break;
                }
            }
        }
    }

    // Also expose the application-configured SSID.
    let mut config_ssid = String::new();
    let config_path = sentryusb_config::find_config_path();
    if let Ok((active, _)) = sentryusb_config::parse_file(config_path) {
        if let Some(v) = active.get("SSID") {
            config_ssid = v.clone();
        }
    }
    // Omit setup placeholders.
    let lower = config_ssid.to_lowercase();
    if matches!(lower.as_str(), "your_ssid" | "yourssid" | "your_wifi" | "ssid" | "your_network" | "") {
        config_ssid.clear();
    }

    let mut wlan_country = String::new();
    if let Ok(out) = sentryusb_shell::run("iw", &["reg", "get"]).await {
        for line in out.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("country") {
                let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
                if parts.len() >= 2 {
                    wlan_country = parts[1].trim_end_matches(':').to_string();
                }
                break;
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "current": {
            "ssid": ssid,
            "connected": connected,
            "source": source,
        },
        "config_ssid": config_ssid,
        "wlan_country": wlan_country,
    })))
}

/// One-pass snapshot summary: count plus the oldest/newest `snap.bin`
/// mtimes (unix seconds).
#[derive(Clone)]
struct SnapshotScan {
    count: usize,
    oldest_unix: Option<u64>,
    newest_unix: Option<u64>,
}

fn cached_snapshot_scan() -> SnapshotScan {
    static SCAN: OnceLock<Mutex<Option<(SnapshotScan, Instant)>>> = OnceLock::new();
    let mut cache = SCAN.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((scan, at)) = &*cache {
        if at.elapsed() < Duration::from_secs(15) { return scan.clone(); }
    }
    let scan = scan_snapshots(std::path::Path::new("/backingfiles/snapshots"));
    *cache = Some((scan.clone(), Instant::now()));
    scan
}

/// Scans only top-level numeric snapshot directories, avoiding autofs symlinks.
fn scan_snapshots(base: &std::path::Path) -> SnapshotScan {
    let mut scan = SnapshotScan { count: 0, oldest_unix: None, newest_unix: None };
    let Ok(entries) = std::fs::read_dir(base) else {
        return scan;
    };
    for entry in entries.flatten() {
        // `file_type` does not follow snapshot autofs symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        // Match the numeric directory contract used by listing and eviction.
        if !entry
            .file_name()
            .to_string_lossy()
            .strip_prefix("snap-")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let snap_bin = entry.path().join("snap.bin");
        // Do not follow an unexpected `snap.bin` symlink.
        if let Ok(meta) = std::fs::symlink_metadata(&snap_bin) {
            scan.count += 1;
            // Slot names are not time-monotonic.
            if let Some(t) = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
            {
                scan.oldest_unix = Some(scan.oldest_unix.map_or(t, |o| o.min(t)));
                scan.newest_unix = Some(scan.newest_unix.map_or(t, |n| n.max(t)));
            }
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_reads_are_identical_and_do_not_consume_sample() {
        let now = Instant::now();
        let baseline = update_net_sample(None, 100, 200, now - Duration::from_secs(1));
        let sample = update_net_sample(Some(&baseline), 1100, 2200, now);
        assert_eq!(sample.rates, Some((8000, 16000)));
        let sampler = Arc::new(Mutex::new(HashMap::from([("wlan0".into(), sample)])));
        let first = latest_throughput(&sampler, "wlan0");
        for _ in 0..20 { assert_eq!(latest_throughput(&sampler, "wlan0").0, first.0); }
        assert_eq!(sampler.lock().unwrap()["wlan0"].taken_at, now);
    }

    #[test]
    fn throughput_distinguishes_idle_reset_warmup_and_stale() {
        let now = Instant::now();
        let first = update_net_sample(None, 100, 200, now);
        assert_eq!(first.rates, None);
        let idle = update_net_sample(Some(&first), 100, 200, now + Duration::from_secs(1));
        assert_eq!(idle.rates, Some((0, 0)));
        let reset = update_net_sample(Some(&idle), 10, 20, now + Duration::from_secs(2));
        assert_eq!(reset.rates, None);
        let resumed = update_net_sample(Some(&reset), 110, 220, now + Duration::from_secs(3));
        assert_eq!(resumed.rates, Some((800, 1600)));
        let mut old = resumed;
        old.taken_at = now - Duration::from_secs(10);
        let sampler = Arc::new(Mutex::new(HashMap::from([("wlan0".into(), old)])));
        assert_eq!(latest_throughput(&sampler, "wlan0").2, "stale");
        assert_eq!(latest_throughput(&sampler, "absent").2, "unavailable");
    }

    #[test]
    fn managed_storage_uses_actual_reserve_and_cleanup_outcome() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(storage_verdict(Some(true), 924 * GIB, 42 * GIB, "healthy", false).0, "healthy");
        assert_eq!(storage_verdict(Some(true), 924 * GIB, 35 * GIB, "recovering", false).0, "recovering");
        assert_eq!(storage_verdict(Some(true), 924 * GIB, 35 * GIB, "failed", false).0, "warn");
        assert_eq!(storage_verdict(Some(true), 32 * GIB, 5 * GIB, "failed", false).0, "warn");
        assert_eq!(storage_verdict(Some(true), 924 * GIB, 0, "healthy", false).0, "fail");
        assert_eq!(storage_verdict(None, 924 * GIB, 42 * GIB, "healthy", false).0, "fail");
        assert_eq!(storage_verdict(Some(false), 924 * GIB, 42 * GIB, "healthy", false).0, "fail");
        assert_eq!(storage_verdict(Some(true), 924 * GIB, 42 * GIB, "healthy", true).0, "fail");
        assert_eq!(mount_writable("/dev/root / ext4 rw 0 0", "/backingfiles"), None);
        assert_eq!(mount_writable("/dev/sda /backingfiles xfs ro,noatime 0 0", "/backingfiles"), Some(false));
    }

    #[test]
    fn scan_snapshots_range_is_mtime_min_max_not_name_order() {
        let base = std::env::temp_dir().join(format!(
            "sentryusb-snap-scan-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let fixtures = [
            ("snap-000000", 1_785_300_000u64),
            ("snap-000413", 1_786_200_000u64),
            ("snap-000414", 1_784_600_000u64),
        ];
        for (name, mtime) in fixtures {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("snap.bin");
            let f = std::fs::File::create(&bin).unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
            f.set_times(std::fs::FileTimes::new().set_modified(t)).unwrap();
        }
        std::fs::File::create(base.join("stray.txt")).unwrap();
        std::fs::create_dir_all(base.join("not-a-snap")).unwrap();
        let decoy = base.join("snap-backup-old");
        std::fs::create_dir_all(&decoy).unwrap();
        let df = std::fs::File::create(decoy.join("snap.bin")).unwrap();
        df.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
        )
        .unwrap();

        let scan = scan_snapshots(&base);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(scan.count, 3);
        assert_eq!(scan.oldest_unix, Some(1_784_600_000));
        assert_eq!(scan.newest_unix, Some(1_786_200_000));
    }

    #[test]
    fn scan_snapshots_empty_dir_reports_none() {
        let base = std::env::temp_dir().join(format!(
            "sentryusb-snap-scan-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let scan = scan_snapshots(&base);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(scan.count, 0);
        assert_eq!(scan.oldest_unix, None);
        assert_eq!(scan.newest_unix, None);
    }
}

fn read_fan_speed() -> String {
    let base = std::path::Path::new("/sys/devices/platform/cooling_fan/hwmon");
    let Ok(entries) = std::fs::read_dir(base) else {
        return String::new();
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("fan1_input");
        if let Ok(data) = std::fs::read_to_string(&candidate) {
            return data.trim().to_string();
        }
    }
    String::new()
}

/// Reads the first UDC's host-link state, shared with the health check.
pub(crate) fn read_udc_state() -> String {
    let Ok(entries) = std::fs::read_dir("/sys/class/udc") else {
        return String::new();
    };
    for entry in entries.flatten() {
        if let Ok(data) = std::fs::read_to_string(entry.path().join("state")) {
            return data.trim().to_string();
        }
    }
    String::new()
}

/// Seconds since the last write to cam_disk.bin, -1 when unknown.
fn cam_last_write_secs() -> i64 {
    let Ok(meta) = std::fs::metadata("/backingfiles/cam_disk.bin") else {
        return -1;
    };
    meta.modified()
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(-1)
}

/// Returns the final hostname segment when it is a plausible device suffix.
fn read_device_suffix() -> String {
    let hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if let Some(idx) = hostname.rfind('-') {
        let suffix = &hostname[idx + 1..];
        if !suffix.is_empty() && suffix.len() <= 8 {
            return suffix.to_string();
        }
    }
    String::new()
}

fn read_net_bytes(dev: &str, stat: &str) -> Option<u64> {
    let path = format!("/sys/class/net/{}/statistics/{}", dev, stat);
    std::fs::read_to_string(&path).ok()?.trim().parse::<u64>().ok()
}

fn update_net_sample(previous: Option<&NetSample>, rx_bytes: u64, tx_bytes: u64, taken_at: Instant) -> NetSample {
    let rates = previous.and_then(|prev| {
        let elapsed = taken_at.saturating_duration_since(prev.taken_at).as_secs_f64();
        if elapsed < 0.5 || elapsed > 5.0 || rx_bytes < prev.rx_bytes || tx_bytes < prev.tx_bytes {
            return None;
        }
        Some((((rx_bytes - prev.rx_bytes) as f64 * 8.0 / elapsed) as u64,
              ((tx_bytes - prev.tx_bytes) as f64 * 8.0 / elapsed) as u64))
    });
    NetSample { rx_bytes, tx_bytes, taken_at, rates }
}

/// One sampling clock serves every web and phone client. HTTP reads never move it.
pub fn start_network_sampler(sampler: NetSampler) {
    static START: Once = Once::new();
    START.call_once(|| {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let sampler = sampler.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return; };
                    let mut values = Vec::new();
                    for entry in entries.flatten() {
                        let dev = entry.file_name().to_string_lossy().into_owned();
                        if dev == "lo" || !iface_is_up(&dev) { continue; }
                        if let (Some(rx), Some(tx)) = (read_net_bytes(&dev, "rx_bytes"), read_net_bytes(&dev, "tx_bytes")) {
                            values.push((dev, rx, tx, Instant::now()));
                        }
                    }
                    let mut map = sampler.lock().unwrap_or_else(|e| e.into_inner());
                    map.retain(|dev, _| values.iter().any(|(name, _, _, _)| name == dev));
                    for (dev, rx, tx, now) in values {
                        let sample = update_net_sample(map.get(&dev), rx, tx, now);
                        map.insert(dev, sample);
                    }
                }).await;
            }
        });
    });
}

fn latest_throughput(sampler: &NetSampler, dev: &str) -> (u64, u64, &'static str, Option<u64>) {
    let map = sampler.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sample) = map.get(dev) else { return (0, 0, "unavailable", None); };
    let age = sample.taken_at.elapsed().as_millis() as u64;
    let quality = if age > 5000 { "stale" } else if sample.rates.is_none() { "sampling" } else { "live" };
    let (rx, tx) = sample.rates.unwrap_or_default();
    (rx, tx, quality, Some(age))
}

pub async fn liveness() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

fn find_net_device(pattern: &str) -> String {
    let prefix = pattern.trim_end_matches('*');
    if let Ok(entries) = std::fs::read_dir("/sys/class/net/") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(prefix) {
                    return name.to_string();
                }
            }
        }
    }
    String::new()
}

/// Gates network commands that may block when an interface is down.
fn iface_is_up(dev: &str) -> bool {
    let path = format!("/sys/class/net/{}/operstate", dev);
    std::fs::read_to_string(&path)
        .map(|s| s.trim() == "up")
        .unwrap_or(false)
}

async fn disk_usage(path: &str) -> i64 {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            return std::fs::metadata(path).map(|m| m.blocks().saturating_mul(512) as i64).unwrap_or(0);
        }
        #[cfg(not(unix))] { let _ = path; 0 }
    }).await.unwrap_or(0)
}

/// Get SBC model from device tree.
pub fn get_sbc_model() -> String {
    for p in &["/proc/device-tree/model", "/sys/firmware/devicetree/base/model"] {
        if let Ok(data) = std::fs::read(p) {
            return String::from_utf8_lossy(&data)
                .trim_end_matches('\0')
                .trim()
                .to_string();
        }
    }
    "unknown".to_string()
}

/// Bundles status, drive stats, and processing status for serialized BLE transport.
pub async fn dashboard_snapshot(
    State(state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let (status, stats, drive_status) = tokio::join!(
        get_status(State(state.clone())),
        crate::drives_handler::drive_stats(State(state.clone())),
        crate::drives_handler::processing_status(State(state)),
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": status.1.0,
            "drive_stats": stats.1.0,
            "drive_status": drive_status.1.0,
        })),
    )
}

//! Push notification pairing + mobile app proxy.
//!
//! Owns the Pi's long-lived `(device_id, device_secret)` credentials used
//! to authenticate against the Sentry Connect backend. Credentials live
//! at `/root/.sentryusb/notification-credentials.json` and are read back
//! by `envsetup.sh` so the bash `send-push-message` wrapper can forward
//! them to the Rust API's `/api/notifications/send` via `MOBILE_PUSH_*`
//! env vars.
//!
//! Pairing-code flow:
//!   1. Sentry Connect requests `POST /api/notifications/generate-code`.
//!   2. Server mints a 6-char alphanumeric code (no ambiguous chars),
//!      registers it with the notification backend, and returns it plus
//!      an expiry timestamp.
//!   3. The user enters the code in Sentry Connect, which contacts the backend
//!      directly to finalize pairing.
//!
//! Paired-device management endpoints are thin proxies — the Pi's only
//! role is to authenticate with its device_secret; the backend owns the
//! per-device state.

use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::router::AppState;

const CREDENTIALS_PATH: &str = "/root/.sentryusb/notification-credentials.json";

/// Alphanumeric charset excluding ambiguous glyphs (0/O, 1/I/l).
const PAIRING_CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIRING_CODE_LEN: usize = 6;
const PAIRING_EXPIRY: Duration = Duration::from_secs(5 * 60);
const MAX_ACTIVE_CODES: usize = 3;

/// Default notification backend. Override with `SENTRY_NOTIFICATION_URL`
/// (env var or `sentryusb.conf` entry).
const DEFAULT_NOTIFICATION_BASE_URL: &str = "https://notifications.sentry-six.com";

fn notification_base_url() -> String {
    // Environment overrides take precedence.
    if let Ok(v) = std::env::var("SENTRY_NOTIFICATION_URL") {
        let trimmed = v.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // systemd does not source sentryusb.conf, so read its override directly.
    let config_path = sentryusb_config::find_config_path();
    if let Ok((active, _)) = sentryusb_config::parse_file(config_path) {
        if let Some(v) = active.get("SENTRY_NOTIFICATION_URL") {
            let trimmed = v.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    DEFAULT_NOTIFICATION_BASE_URL.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationCredentials {
    device_id: String,
    device_secret: String,
}

static CACHED_CREDS: OnceLock<NotificationCredentials> = OnceLock::new();

/// Load or generate credentials, caching the immutable pair for this process.
fn get_or_create_credentials() -> Option<&'static NotificationCredentials> {
    if let Some(existing) = CACHED_CREDS.get() {
        return Some(existing);
    }

    if let Ok(data) = std::fs::read_to_string(CREDENTIALS_PATH) {
        if let Ok(c) = serde_json::from_str::<NotificationCredentials>(&data) {
            if !c.device_id.is_empty() && !c.device_secret.is_empty() {
                let _ = CACHED_CREDS.set(c);
                return CACHED_CREDS.get();
            }
        }
    }

    // 32 bytes yields a 64-character ID; 64 bytes yields a 128-character secret.
    let device_id = random_hex(32);
    let device_secret = random_hex(64);
    let new = NotificationCredentials { device_id, device_secret };

    // Persist credentials on the normally read-only root filesystem.
    let _ = std::process::Command::new("bash")
        .args(["-c", "/root/bin/remountfs_rw"])
        .status();

    if let Some(dir) = Path::new(CREDENTIALS_PATH).parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!("[notifications] failed to mkdir {}: {}", dir.display(), e);
        }
        // The parent contains a bearer secret.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }

    match serde_json::to_vec_pretty(&new) {
        Ok(data) => {
            if let Err(e) = write_private(CREDENTIALS_PATH, &data) {
                warn!("[notifications] failed to save credentials: {}", e);
                return None;
            }
            let short = new.device_id.chars().take(8).collect::<String>();
            info!("[notifications] Generated new device credentials: {}", short);
            let _ = CACHED_CREDS.set(new);
            CACHED_CREDS.get()
        }
        Err(e) => {
            warn!("[notifications] failed to serialize new credentials: {}", e);
            None
        }
    }
}

/// Write file at `path` with 0600 permissions via tmp + rename.
fn write_private(path: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = format!("{}.tmp", path);
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn random_hex(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Enable mobile push after pairing without delaying the pairing response.
async fn auto_enable_mobile_push_in_config() {
    // Best-effort; write only active configuration values.
    tokio::task::spawn_blocking(|| {
        let config_path = sentryusb_config::find_config_path();
        let (mut active, _) = match sentryusb_config::parse_file(config_path) {
            Ok(v) => v,
            Err(_) => return,
        };
        if active.get("MOBILE_PUSH_ENABLED").map(|v| v.as_str()) == Some("true") {
            return;
        }
        active.insert("MOBILE_PUSH_ENABLED".to_string(), "true".to_string());
        let _ = std::process::Command::new("bash")
            .args(["-c", "/root/bin/remountfs_rw"])
            .status();
        match sentryusb_config::write_file(config_path, &active) {
            Ok(()) => info!("[notifications] Auto-enabled MOBILE_PUSH_ENABLED in config"),
            Err(e) => warn!("[notifications] Failed to enable MOBILE_PUSH_ENABLED in config: {}", e),
        }
    })
    .await
    .ok();
}

#[derive(Clone)]
struct PairingCode {
    code: String,
    expires_at: SystemTime,
}

static ACTIVE_CODES: Mutex<Vec<PairingCode>> = Mutex::new(Vec::new());

fn generate_pairing_code_string() -> String {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(PAIRING_CODE_LEN);
    for _ in 0..PAIRING_CODE_LEN {
        let idx = (rng.next_u32() as usize) % PAIRING_CHARSET.len();
        out.push(PAIRING_CHARSET[idx] as char);
    }
    out
}

fn clean_expired_codes(codes: &mut Vec<PairingCode>) {
    let now = SystemTime::now();
    codes.retain(|c| c.expires_at > now);
}

fn to_rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// POST /api/notifications/generate-code
pub async fn generate_pairing_code(
    State(_s): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let creds = match get_or_create_credentials() {
        Some(c) => c,
        None => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to initialize notification credentials",
            );
        }
    };

    // Expose a code only after the backend acknowledges it.
    let (code, expires_at) = {
        let mut codes = ACTIVE_CODES.lock().unwrap_or_else(|p| p.into_inner());
        clean_expired_codes(&mut codes);
        if codes.len() >= MAX_ACTIVE_CODES {
            return crate::json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many active pairing codes. Wait for existing codes to expire.",
            );
        }
        let code = generate_pairing_code_string();
        let expires_at = SystemTime::now() + PAIRING_EXPIRY;
        codes.push(PairingCode { code: code.clone(), expires_at });
        (code, expires_at)
    };

    match register_code_with_backend(creds, &code).await {
        Ok(()) => {
            info!(
                "[notifications] Generated pairing code {} (expires {})",
                code,
                to_rfc3339(expires_at)
            );
            tokio::spawn(auto_enable_mobile_push_in_config());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "code": code,
                    "expires_at": to_rfc3339(expires_at),
                })),
            )
        }
        Err(e) => {
            // Failed registrations must not consume an active-code slot.
            let mut codes = ACTIVE_CODES.lock().unwrap_or_else(|p| p.into_inner());
            codes.retain(|c| c.code != code);
            warn!("[notifications] Failed to register code {} with backend: {}", code, e);
            crate::json_error(
                StatusCode::BAD_GATEWAY,
                "Failed to register pairing code with notification server. Check internet connection.",
            )
        }
    }
}

async fn register_code_with_backend(
    creds: &NotificationCredentials,
    code: &str,
) -> Result<(), String> {
    let hostname = tokio::process::Command::new("hostname")
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // Privacy: fingerprint deliberately omitted. The notification pairing
    // identifies this device via its randomly-generated `device_id`; we no
    // longer cross-link it to the telemetry-side hardware fingerprint.
    let body = serde_json::json!({
        "device_id": creds.device_id,
        "device_secret": creds.device_secret,
        "code": code,
        "hostname": hostname,
    });

    let url = format!("{}/register-code", notification_base_url());
    let resp = crate::http_client()
        .post(&url)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("failed to reach notification server: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("backend returned {}: {}", status, body));
    }

    info!("[notifications] Code {} registered with backend successfully", code);
    Ok(())
}

/// GET /api/notifications/paired-devices
///
/// Proxies `GET {base}/devices?device_id=X` with `X-Device-Secret`. The
/// backend is authoritative — we don't keep a local device list, because
/// a device can unpair from Sentry Connect without touching the Pi.
pub async fn list_paired_devices(
    State(_s): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let creds = match get_or_create_credentials() {
        Some(c) => c,
        None => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Notification credentials not available",
            );
        }
    };

    let url = format!(
        "{}/devices?device_id={}",
        notification_base_url(),
        creds.device_id
    );
    let resp = match crate::http_client()
        .get(&url)
        .timeout(Duration::from_secs(10))
        .header("X-Device-Secret", &creds.device_secret)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            return crate::json_error(
                StatusCode::BAD_GATEWAY,
                "Failed to reach notification backend",
            );
        }
    };

    proxy_response(resp).await
}

/// DELETE /api/notifications/paired-devices/{id}
pub async fn remove_paired_device(
    State(_s): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if id.is_empty() {
        return crate::json_error(StatusCode::BAD_REQUEST, "Missing pairing ID");
    }

    let creds = match get_or_create_credentials() {
        Some(c) => c,
        None => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Notification credentials not available",
            );
        }
    };

    let url = format!(
        "{}/devices/{}?device_id={}",
        notification_base_url(),
        id,
        creds.device_id
    );
    let resp = match crate::http_client()
        .delete(&url)
        .timeout(Duration::from_secs(10))
        .header("X-Device-Secret", &creds.device_secret)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            return crate::json_error(
                StatusCode::BAD_GATEWAY,
                "Failed to reach notification backend",
            );
        }
    };

    info!("[notifications] Removed paired device: {}", id);
    proxy_response(resp).await
}

/// Forward backend status and JSON, wrapping non-JSON bodies.
async fn proxy_response(resp: reqwest::Response) -> (StatusCode, Json<serde_json::Value>) {
    let status = resp.status();
    let bytes = resp.bytes().await.unwrap_or_default();
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::json!({ "raw": String::from_utf8_lossy(&bytes).into_owned() })
    });
    (status_code, Json(body))
}

/// POST /api/notifications/test
///
/// Sends a test notification to the mobile push backend only.
/// Exclusively targets the
/// Sentry Connect relay — other providers are not exercised here.
pub async fn send_test_notification(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let creds = match get_or_create_credentials() {
        Some(c) => c,
        None => return crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Notification credentials not available"),
    };

    let hostname = std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "SentryUSB".to_string());
    let hostname = hostname.trim();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let result = sentryusb_notify::sentry_connect::send(
        &client,
        &creds.device_id,
        &creds.device_secret,
        "SentryUSB Test",
        &format!("Test notification from {}.", hostname),
    ).await;

    let (status_code, body, send_ok, error_msg) = match result {
        Ok(()) => {
            info!("[notifications] Test notification sent successfully");
            (
                StatusCode::OK,
                serde_json::json!({"status": "ok"}),
                true,
                String::new(),
            )
        }
        Err(e) => {
            let msg = sentryusb_notify::safe_provider_error(&e, &[&creds.device_secret]);
            warn!("[notifications] Test notification failed: {}", msg);
            (
                StatusCode::BAD_GATEWAY,
                serde_json::json!({"status": "error", "error": msg.clone()}),
                false,
                msg,
            )
        }
    };

    // Test sends appear in history alongside runtime notifications.
    let mut results_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    results_map.insert(
        "sentry_connect".to_string(),
        if send_ok { "ok".to_string() } else { format!("error: {}", error_msg) },
    );
    let event = crate::notification_center::NotificationEvent {
        id: String::new(),
        timestamp: 0,
        event_type: "test".to_string(),
        title: "SentryUSB Test".to_string(),
        message: format!("Test notification from {} — push notifications are working!", hostname),
        summary: Some(format!("Test notification from {}.", hostname)),
        provider_errors: if send_ok { Default::default() } else {
            std::collections::HashMap::from([("sentry_connect".to_string(), error_msg)])
        },
        providers: vec!["sentry_connect".to_string()],
        results: results_map,
    };
    if let Err(e) = crate::notification_center::record_event(event) {
        warn!("[notifications] Failed to record test notification in history: {}", e);
    }

    (status_code, Json(body))
}

/// Body for `POST /api/notifications/send`.
///
/// Fields correspond to the `send-push-message` wrapper arguments:
///   * `title`, `message` — required.
///   * `type_hint` (`start` / `finish`) — for the live_activity branch
///     on mobile push.
///   * `notification_type` (`archive_start`, `temperature`, …) — used
///     for the gate check and echoed in mobile push + history.
///   * `archive_total_count` — live_activity payload on archive_start.
#[derive(Deserialize)]
pub struct SendNotificationRequest {
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default, rename = "type")]
    pub type_hint: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub archive_total_count: Option<u32>,
}

#[cfg(test)]
mod notification_summary_tests {
    use super::*;

    #[test]
    fn notification_summary_is_external_only_and_optional_for_old_callers() {
        let old: SendNotificationRequest = serde_json::from_value(serde_json::json!({
            "title": "SentryUSB", "message": "full diagnostic"
        })).unwrap();
        assert_eq!(outgoing_message(&old.message, old.summary.as_deref()), "full diagnostic");
        let new: SendNotificationRequest = serde_json::from_value(serde_json::json!({
            "title": "SentryUSB", "message": "full diagnostic", "summary": "Short alert",
            "type": "start", "notification_type": "archive_start", "archive_total_count": 13
        })).unwrap();
        assert_eq!(outgoing_message(&new.message, new.summary.as_deref()), "Short alert");
        assert_eq!(new.message, "full diagnostic");
        assert_eq!(new.archive_total_count, Some(13));
        assert_eq!(new.type_hint.as_deref(), Some("start"));
        assert_eq!(outgoing_message("full diagnostic", Some("  ")), "full diagnostic");
    }

    #[tokio::test]
    async fn notification_webhook_receives_summary_but_history_keeps_details_and_failure() {
        use std::io::{Read, Write};
        for status in ["200 OK", "400 Bad Request"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut raw = Vec::new();
                let mut byte = [0u8; 1];
                while !raw.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    raw.push(byte[0]);
                }
                let headers = String::from_utf8(raw).unwrap();
                let size: usize = headers.lines().find_map(|line| {
                    line.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse().unwrap())
                }).unwrap();
                let mut body = vec![0; size];
                socket.read_exact(&mut body).unwrap();
                let response = "chat not found";
                write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()
            });
            let config = sentryusb_notify::NotifyConfig {
                webhook_enabled: true,
                webhook_url: format!("http://{address}/private-capability"),
                ..Default::default()
            };
            let request: SendNotificationRequest = serde_json::from_value(serde_json::json!({
                "title":"SentryUSB", "message":"archive-clips.sh exit code 23",
                "summary":"Archive interrupted.", "notification_type":"archive_error", "type":"finish"
            })).unwrap();
            let (event, outcome) = dispatch_to_providers(&config, &request).await;
            let payload = server.join().unwrap();
            assert_eq!(payload["value1"], "SentryUSB");
            assert_eq!(payload["value2"], "Archive interrupted.");
            assert_eq!(event.message, "archive-clips.sh exit code 23");
            assert_eq!(event.summary.as_deref(), Some("Archive interrupted."));
            assert_eq!(event.event_type, "archive_error");
            assert_eq!(outcome.providers, ["webhook"]);
            if status.starts_with("400") {
                assert_eq!(event.results["webhook"], "error");
                assert!(event.provider_errors["webhook"].contains("400"));
                assert!(event.provider_errors["webhook"].contains("chat not found"));
                assert_eq!(outcome.failures.len(), 1);
            } else {
                assert_eq!(event.results["webhook"], "ok");
                assert!(event.provider_errors.is_empty());
                assert!(outcome.failures.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn notification_no_configured_provider_still_has_detailed_history() {
        let request = serde_json::from_value(serde_json::json!({
            "title": "SentryUSB", "message": "full message"
        })).unwrap();
        let (event, outcome) = dispatch_to_providers(&Default::default(), &request).await;
        assert!(outcome.providers.is_empty());
        assert_eq!(event.message, "full message");
        assert!(event.summary.is_none());
    }

    #[tokio::test]
    async fn notification_transport_failure_is_returned_even_when_history_write_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener); // A refused local connection; no external traffic.
        let config = sentryusb_notify::NotifyConfig {
            webhook_enabled: true, webhook_url: format!("http://{address}/private-token"),
            ..Default::default()
        };
        let request = serde_json::from_value(serde_json::json!({
            "title":"SentryUSB", "message":"full archive exit code 23", "summary":"Archive interrupted."
        })).unwrap();
        let recorded = std::cell::Cell::new(false);
        let outcome = dispatch_and_record_using(&config, &request, |event| {
            assert_eq!(event.message, "full archive exit code 23");
            assert!(event.provider_errors["webhook"].to_lowercase().contains("connect"));
            assert!(!event.provider_errors["webhook"].contains("private-token"));
            recorded.set(true);
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "history is read-only"))
        }).await;
        assert!(recorded.get());
        assert_eq!(outcome.providers, ["webhook"]);
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].to_lowercase().contains("connect"));
    }
}

/// Per-provider outcome summary from a dispatch, pre-digested so callers
/// don't need to name the notify crate's error types.
pub(crate) struct DispatchOutcome {
    pub providers: Vec<String>,
    /// "Provider: error text" for each failure
    pub failures: Vec<String>,
}

/// Internal (non-HTTP) notification dispatch: gate by type, fan out to
/// every configured provider, record a history event. Returns `None`
/// when the type is disabled in notification settings (nothing sent).
/// This is the same pipeline as `POST /api/notifications/send`, callable
/// from inside the server (boot-time storage repair).
pub(crate) async fn dispatch_and_record(
    title: &str,
    message: &str,
    notification_type: Option<&str>,
    type_hint: Option<&str>,
    archive_total_count: Option<u32>,
    summary: Option<&str>,
) -> Option<DispatchOutcome> {
    if !crate::notification_center::is_type_enabled(notification_type) {
        return None;
    }

    let config = sentryusb_notify::NotifyConfig::from_config();
    let request = SendNotificationRequest {
        title: title.to_string(),
        message: message.to_string(),
        summary: summary.map(str::to_string),
        notification_type: notification_type.map(str::to_string),
        type_hint: type_hint.map(str::to_string),
        archive_total_count,
    };
    Some(dispatch_and_record_using(&config, &request, crate::notification_center::record_event).await)
}

async fn dispatch_and_record_using(
    config: &sentryusb_notify::NotifyConfig,
    request: &SendNotificationRequest,
    record: impl FnOnce(crate::notification_center::NotificationEvent) -> std::io::Result<crate::notification_center::NotificationEvent>,
) -> DispatchOutcome {
    let (event, outcome) = dispatch_to_providers(config, request).await;
    if let Err(e) = record(event) {
        tracing::warn!("[notifications] Failed to record history event: {}", e);
    }
    outcome
}

/// Dispatch and describe the event separately from persistence, so a full or
/// damaged history file cannot prevent a notification from being attempted.
async fn dispatch_to_providers(
    config: &sentryusb_notify::NotifyConfig,
    request: &SendNotificationRequest,
) -> (crate::notification_center::NotificationEvent, DispatchOutcome) {
    let title = request.title.as_str();
    let message = request.message.as_str();
    let summary = request.summary.as_deref();
    let notification_type = request.notification_type.as_deref();
    let req = sentryusb_notify::NotifyRequest {
        title,
        message: outgoing_message(message, summary),
        type_hint: request.type_hint.as_deref(),
        notification_type,
        archive_total_count: request.archive_total_count,
    };
    let results = sentryusb_notify::send_to_all_with_context(config, &req).await;

    // Build per-provider history results.
    let mut providers: Vec<String> = Vec::with_capacity(results.len());
    let mut result_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(results.len());
    let mut failures: Vec<String> = Vec::new();
    let mut provider_errors = std::collections::HashMap::new();
    for (name, res) in &results {
        providers.push(name.clone());
        match res {
            Ok(()) => {
                result_map.insert(name.clone(), "ok".to_string());
            }
            Err(e) => {
                result_map.insert(name.clone(), "error".to_string());
                provider_errors.insert(name.clone(), e.to_string());
                failures.push(format!("{}: {}", name, e));
            }
        }
    }

    // Unspecified types are recorded as `general`.
    let event = crate::notification_center::NotificationEvent {
        id: String::new(),
        timestamp: 0,
        event_type: notification_type.unwrap_or("general").to_string(),
        title: title.to_string(),
        message: message.to_string(),
        summary: summary.filter(|s| !s.trim().is_empty()).map(str::to_string),
        provider_errors,
        providers: providers.clone(),
        results: result_map,
    };
    (event, DispatchOutcome { providers, failures })
}

/// POST /api/notifications/send
///
/// Single entry point used by the runtime scripts (archiveloop,
/// temperature_monitor, post-archive-process.sh, …) via the
/// `/root/bin/send-push-message` curl wrapper:
///   1. Gate-check the notification_type against user settings. If
///      disabled, return `{"skipped": true, "reason": "type_disabled"}`
///      without touching any provider (no history event written).
///   2. Dispatch to every configured notifier via the Rust notify crate.
///   3. Record a history event with the per-provider results.
pub async fn send_notification(
    State(_s): State<AppState>,
    Json(body): Json<SendNotificationRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let notification_type = body.notification_type.as_deref();
    let Some(out) = dispatch_and_record(
        &body.title,
        &body.message,
        notification_type,
        body.type_hint.as_deref(),
        body.archive_total_count,
        body.summary.as_deref(),
    )
    .await
    else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "skipped": true,
                "reason": "type_disabled",
                "type": notification_type.unwrap_or(""),
            })),
        );
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "attempted": out.providers.len(),
            "providers": out.providers,
            "failed": out.failures,
        })),
    )
}

fn outgoing_message<'a>(message: &'a str, summary: Option<&'a str>) -> &'a str {
    summary.filter(|value| !value.trim().is_empty()).unwrap_or(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_codes_use_allowed_charset_only() {
        let code = generate_pairing_code_string();
        assert_eq!(code.len(), PAIRING_CODE_LEN);
        for c in code.bytes() {
            assert!(
                PAIRING_CHARSET.contains(&c),
                "unexpected char {:?} in pairing code {:?}",
                c as char,
                code
            );
        }
    }

    #[test]
    fn pairing_code_excludes_ambiguous_chars() {
        for banned in b"0OI" {
            assert!(
                !PAIRING_CHARSET.contains(banned),
                "pairing charset must exclude {:?}",
                *banned as char
            );
        }
    }

    #[test]
    fn random_hex_has_twice_the_byte_length() {
        for n in [0, 1, 16, 32, 64] {
            assert_eq!(random_hex(n).len(), n * 2);
        }
    }

    #[test]
    fn expired_codes_are_dropped() {
        let mut codes = vec![
            PairingCode {
                code: "OLD".into(),
                expires_at: SystemTime::now() - Duration::from_secs(1),
            },
            PairingCode {
                code: "NEW".into(),
                expires_at: SystemTime::now() + Duration::from_secs(300),
            },
        ];
        clean_expired_codes(&mut codes);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].code, "NEW");
    }
}

//! Notification center: history and type settings.
//!
//! - History events carry id, unix-ts, type, title, message, providers,
//!   per-provider results.
//! - Newest-first ordering, max 500 entries.
//! - Query params: `limit`, `offset`, `type` (filter).
//! - Settings are stored in the user-preferences map with `notify_<type>` keys.
//! - The previous history filename remains a read-only migration fallback.

use std::collections::HashMap;
use std::sync::RwLock;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::router::AppState;

const HISTORY_PATH: &str = "/mutable/sentryusb-notifications.json";
const LEGACY_HISTORY_PATH: &str = "/mutable/.notification_history.json";
const MAX_HISTORY: usize = 500;

static HISTORY_LOCK: RwLock<()> = RwLock::new(());

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct NotificationEvent {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "ts", default)]
    pub timestamp: i64,
    #[serde(rename = "type", default)]
    pub event_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub message: String,
    /// Short external alert; message remains the diagnostic text for old clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_errors: HashMap<String, String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub results: HashMap<String, String>,
}

fn load_history_from(path: &std::path::Path, legacy: &std::path::Path) -> std::io::Result<Vec<NotificationEvent>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match std::fs::read_to_string(legacy) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    };
    serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn load_history_locked() -> std::io::Result<Vec<NotificationEvent>> {
    load_history_from(std::path::Path::new(HISTORY_PATH), std::path::Path::new(LEGACY_HISTORY_PATH))
}

fn save_history_locked(events: &[NotificationEvent]) -> std::io::Result<()> {
    save_history_to(std::path::Path::new(HISTORY_PATH), events)
}

fn save_history_to(path: &std::path::Path, events: &[NotificationEvent]) -> std::io::Result<()> {
    use std::io::Write;
    let data = serde_json::to_vec_pretty(events)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    // HISTORY_LOCK serializes writers; create_new also avoids following a stale
    // temporary symlink. A failed write never truncates the existing history.
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_nanos();
    let temporary = path.with_extension(format!("{}.{nonce}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let result = (|| {
        file.write_all(&data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().unwrap_or(std::path::Path::new(".")))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct NotificationSettings {
    pub archive_start: bool,
    pub archive_complete: bool,
    pub archive_error: bool,
    pub temperature: bool,
    #[serde(rename = "keep_awake_failure")]
    pub keep_awake: bool,
    pub update: bool,
    pub drives: bool,
    pub rtc_battery: bool,
    pub music_sync: bool,
    /// Keep-Accessory automation events (e.g. "Pi going offline" when the
    /// 12V outlet is released at home). Only relevant for 12V-powered Pis.
    #[serde(default = "default_true")]
    pub keep_accessory: bool,
    /// Boot-time storage auto repair events (success/failure/needs-manual).
    #[serde(default = "default_true")]
    pub storage_repair: bool,
}

fn default_true() -> bool {
    true
}

impl Default for NotificationSettings {
    fn default() -> Self {
        NotificationSettings {
            archive_start: true,
            archive_complete: true,
            archive_error: true,
            temperature: true,
            keep_awake: true,
            update: true,
            drives: true,
            rtc_battery: true,
            music_sync: true,
            keep_accessory: true,
            storage_repair: true,
        }
    }
}

pub(crate) fn bool_pref(prefs: &serde_json::Map<String, serde_json::Value>, key: &str, default: bool) -> bool {
    match prefs.get(key) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => default,
    }
}

fn load_settings() -> NotificationSettings {
    let prefs = crate::preferences::load_prefs();
    NotificationSettings {
        archive_start: bool_pref(&prefs, "notify_archive_start", true),
        archive_complete: bool_pref(&prefs, "notify_archive_complete", true),
        archive_error: bool_pref(&prefs, "notify_archive_error", true),
        temperature: bool_pref(&prefs, "notify_temperature", true),
        keep_awake: bool_pref(&prefs, "notify_keep_awake_failure", true),
        update: bool_pref(&prefs, "notify_update", true),
        drives: bool_pref(&prefs, "notify_drives", true),
        rtc_battery: bool_pref(&prefs, "notify_rtc_battery", true),
        music_sync: bool_pref(&prefs, "notify_music_sync", true),
        keep_accessory: bool_pref(&prefs, "notify_keep_accessory", true),
        storage_repair: bool_pref(&prefs, "notify_storage_repair", true),
    }
}

fn save_settings(store:&sentryusb_drives::DriveStore,s:&NotificationSettings)->anyhow::Result<bool> {
    crate::preferences::edit_prefs(store,|prefs| {
    let put = |prefs: &mut serde_json::Map<String, serde_json::Value>, k: &str, v: bool| {
        prefs.insert(k.to_string(), serde_json::Value::String(
            if v { "true".to_string() } else { "false".to_string() },
        ));
    };
    put(prefs, "notify_archive_start", s.archive_start);
    put(prefs, "notify_archive_complete", s.archive_complete);
    put(prefs, "notify_archive_error", s.archive_error);
    put(prefs, "notify_temperature", s.temperature);
    put(prefs, "notify_keep_awake_failure", s.keep_awake);
    put(prefs, "notify_update", s.update);
    put(prefs, "notify_drives", s.drives);
    put(prefs, "notify_rtc_battery", s.rtc_battery);
    put(prefs, "notify_music_sync", s.music_sync);
    put(prefs, "notify_keep_accessory", s.keep_accessory);
    put(prefs, "notify_storage_repair", s.storage_repair);
    Ok(true)
    })
}

/// GET /api/notifications/settings
pub async fn get_settings(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(serde_json::to_value(load_settings()).unwrap_or_default()))
}

/// PUT /api/notifications/settings
pub async fn update_settings(
    State(state): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    let settings: NotificationSettings = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(_) => return crate::json_error(StatusCode::BAD_REQUEST, "Invalid request body"),
    };
    let store=state.drives.store.clone();
    match tokio::task::spawn_blocking(move||save_settings(&store,&settings)).await {
        Ok(Ok(_))=>crate::json_ok(),
        _=>crate::json_error(StatusCode::INTERNAL_SERVER_ERROR,"Notification settings could not be saved."),
    }
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /api/notifications/history
pub async fn get_history(
    State(_s): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let _guard = HISTORY_LOCK.read().unwrap_or_else(|p| p.into_inner());
    let mut events = match load_history_locked() {
        Ok(events) => events,
        Err(e) => return crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to read notification history: {e}")),
    };
    if let Some(t) = &q.event_type {
        if !t.is_empty() {
            events.retain(|e| &e.event_type == t);
        }
    }
    let total = events.len();
    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);
    let page: Vec<NotificationEvent> = if offset >= events.len() {
        Vec::new()
    } else {
        let end = offset.saturating_add(limit).min(events.len());
        events[offset..end].to_vec()
    };
    (StatusCode::OK, Json(serde_json::json!({
        "events": page,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// POST /api/notifications/history
pub async fn append_history(
    State(_s): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    let event: NotificationEvent = match serde_json::from_str(&body) {
        Ok(e) => e,
        Err(_) => return crate::json_error(StatusCode::BAD_REQUEST, "Invalid event data"),
    };
    match record_event(event) {
        Ok(saved) => (StatusCode::OK, Json(serde_json::to_value(saved).unwrap_or_default())),
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to save notification history: {}", e),
        ),
    }
}

/// Prepend `event` to the history file and save. Usable from other
/// handlers without going through HTTP self-calls. Fills in `id` +
/// `timestamp` if the caller left them zero/empty.
///
/// Returns the event as persisted (with any auto-filled fields).
pub(crate) fn record_event(
    event: NotificationEvent,
) -> std::io::Result<NotificationEvent> {
    let _guard = HISTORY_LOCK.write().unwrap_or_else(|p| p.into_inner());
    record_event_in(event, std::path::Path::new(HISTORY_PATH), std::path::Path::new(LEGACY_HISTORY_PATH))
}

fn record_event_in(
    event: NotificationEvent,
    path: &std::path::Path,
    legacy: &std::path::Path,
) -> std::io::Result<NotificationEvent> {
    record_event_in_mode(event, path, legacy, false)
}

pub(crate) fn record_update_event(event: NotificationEvent) -> std::io::Result<NotificationEvent> {
    let _guard = HISTORY_LOCK.write().unwrap_or_else(|p| p.into_inner());
    record_event_in_mode(event, std::path::Path::new(HISTORY_PATH), std::path::Path::new(LEGACY_HISTORY_PATH), true)
}

fn record_event_in_mode(
    mut event: NotificationEvent,
    path: &std::path::Path,
    legacy: &std::path::Path,
    replace_id: bool,
) -> std::io::Result<NotificationEvent> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if event.timestamp == 0 {
        event.timestamp = now;
    }
    if event.id.is_empty() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        event.id = to_base36(nanos);
    }

    let mut events = load_history_from(path, legacy)?;
    // Durable update delivery retries use one stable ID, not a new history row.
    if replace_id { events.retain(|existing| existing.id != event.id); }
    events.insert(0, event.clone());
    if events.len() > MAX_HISTORY {
        events.truncate(MAX_HISTORY);
    }
    save_history_to(path, &events)?;
    Ok(event)
}

/// Evaluate the notification-type gate for a given type.
/// `None` or empty is always allowed.
pub(crate) fn is_type_enabled(notification_type: Option<&str>) -> bool {
    let Some(ntype) = notification_type else { return true };
    if ntype.is_empty() {
        return true;
    }
    let s = load_settings();
    match ntype {
        "archive_start" => s.archive_start,
        "archive_complete" => s.archive_complete,
        "archive_error" => s.archive_error,
        "temperature" => s.temperature,
        "keep_awake_failure" => s.keep_awake,
        "update" => s.update,
        "drives" => s.drives,
        "rtc_battery" => s.rtc_battery,
        "music_sync" => s.music_sync,
        "keep_accessory" => s.keep_accessory,
        "storage_repair" => s.storage_repair,
        _ => true,
    }
}

/// DELETE /api/notifications/history
pub async fn clear_history(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let _guard = HISTORY_LOCK.write().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = save_history_locked(&[]) {
        return crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to clear notification history: {}", e),
        );
    }
    crate::json_ok()
}

/// DELETE /api/notifications/history/{id}
pub async fn delete_history_item(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if id.is_empty() {
        return crate::json_error(StatusCode::BAD_REQUEST, "Missing notification ID");
    }
    let _guard = HISTORY_LOCK.write().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = delete_history_from(std::path::Path::new(HISTORY_PATH), std::path::Path::new(LEGACY_HISTORY_PATH), &id) {
        return crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to save notification history: {}", e),
        );
    }
    crate::json_ok()
}

fn delete_history_from(path: &std::path::Path, legacy: &std::path::Path, id: &str) -> std::io::Result<()> {
    let mut events = load_history_from(path, legacy)?;
    events.retain(|e| e.id != id);
    save_history_to(path, &events)
}

#[derive(Deserialize)]
pub struct CheckParams {
    #[serde(rename = "type")]
    pub notification_type: Option<String>,
}

/// GET /api/notifications/settings/check?type=archive_start
///
/// Return `{type, enabled}` for the user's notification toggle.
pub async fn check_notification_type(
    State(_s): State<AppState>,
    Query(params): Query<CheckParams>,
) -> (StatusCode, Json<serde_json::Value>) {
    let ntype = match params.notification_type.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return crate::json_error(StatusCode::BAD_REQUEST, "Missing type parameter"),
    };
    let s = load_settings();
    let enabled = match ntype {
        "archive_start" => s.archive_start,
        "archive_complete" => s.archive_complete,
        "archive_error" => s.archive_error,
        "temperature" => s.temperature,
        "keep_awake_failure" => s.keep_awake,
        "update" => s.update,
        "drives" => s.drives,
        "rtc_battery" => s.rtc_battery,
        "music_sync" => s.music_sync,
        "keep_accessory" => s.keep_accessory,
        "storage_repair" => s.storage_repair,
        _ => true,
    };
    (StatusCode::OK, Json(serde_json::json!({"type": ntype, "enabled": enabled})))
}

fn to_base36(mut n: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_corrupt_history_rejects_append_and_dismiss_without_losing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let legacy = dir.path().join("legacy.json");
        std::fs::write(&path, "[broken-but-recoverable").unwrap();
        assert!(record_event_in(NotificationEvent::default(), &path, &legacy).is_err());
        assert!(delete_history_from(&path, &legacy, "old").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "[broken-but-recoverable");
    }

    #[test]
    fn notification_atomic_write_preserves_old_reader_and_ignores_abandoned_temp() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let legacy = dir.path().join("legacy.json");
        let old = r#"[{"id":"old","message":"original"}]"#;
        std::fs::write(&path, old).unwrap();
        std::fs::write(dir.path().join("history.abandoned.tmp"), "[partial").unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();
        record_event_in(NotificationEvent { message: "new".into(), ..Default::default() }, &path, &legacy).unwrap();
        let mut old_bytes = String::new();
        reader.read_to_string(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, old, "saving must replace, not truncate the existing inode");
        assert_eq!(load_history_from(&path, &legacy).unwrap().len(), 2);
    }

    #[test]
    fn notification_failed_publish_leaves_destination_and_removes_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::create_dir(&path).unwrap(); // rename cannot replace a directory
        std::fs::write(path.join("recoverable"), "original").unwrap();
        assert!(save_history_to(&path, &[NotificationEvent::default()]).is_err());
        assert_eq!(std::fs::read_to_string(path.join("recoverable")).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn notification_denied_write_preserves_existing_history() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let legacy = dir.path().join("legacy.json");
        let old = r#"[{"id":"old","message":"original"}]"#;
        std::fs::write(&path, old).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = record_event_in(NotificationEvent::default(), &path, &legacy);
        // Privileged/root test runners can bypass mode bits. This permission
        // case is exercised on ordinary Linux runners; the publish-failure
        // case above is independent of privilege.
        let bypasses_permissions = std::fs::write(dir.path().join("probe"), "probe").is_ok();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if bypasses_permissions { return; }
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), old);
    }

    #[test]
    fn notification_legacy_event_keeps_details_and_new_fields_are_optional() {
        let event: NotificationEvent = serde_json::from_str(
            r#"{"id":"old","ts":1,"type":"archive_error","title":"SentryUSB","message":"exit code 23","providers":["telegram"],"results":{"telegram":"error"}}"#
        ).unwrap();
        assert!(event.summary.is_none());
        assert!(event.provider_errors.is_empty());
        assert_eq!(event.message, "exit code 23");
    }

    #[test]
    fn notification_history_roundtrip_and_corrupt_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let legacy = dir.path().join("legacy.json");
        let old = r#"[{"id":"old","message":"full old message"}]"#;
        std::fs::write(&legacy, old).unwrap();
        let mut events = load_history_from(&path, &legacy).unwrap();
        assert_eq!(events[0].message, "full old message");
        events.insert(0, NotificationEvent {
            message: "HTTP 400: chat not found".into(),
            summary: Some("Notification failed".into()),
            provider_errors: HashMap::from([("telegram".into(), "HTTP 400: chat not found".into())]),
            ..Default::default()
        });
        save_history_to(&path, &events).unwrap();
        let saved = load_history_from(&path, &legacy).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].message, "HTTP 400: chat not found");
        assert_eq!(saved[0].summary.as_deref(), Some("Notification failed"));
        assert_eq!(saved[0].provider_errors["telegram"], "HTTP 400: chat not found");
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), old);
        std::fs::write(&path, "[broken").unwrap();
        assert!(load_history_from(&path, &legacy).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[broken");
    }

    #[test]
    fn notification_history_missing_is_empty_but_empty_existing_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let legacy = dir.path().join("legacy.json");
        assert!(load_history_from(&path, &legacy).unwrap().is_empty());
        std::fs::write(&path, "").unwrap();
        assert!(load_history_from(&path, &legacy).is_err());
        save_history_to(&path, &[]).unwrap(); // Explicit Clear can repair history.
        assert!(load_history_from(&path, &legacy).unwrap().is_empty());
    }

    #[test]
    fn settings_deserialize_defaults_storage_repair_on() {
        // Omitted storage-repair settings remain enabled.
        let json = r#"{
            "archive_start": true, "archive_complete": true,
            "archive_error": true, "temperature": true,
            "keep_awake_failure": true, "update": true, "drives": true,
            "rtc_battery": true, "music_sync": true, "keep_accessory": true
        }"#;
        let s: NotificationSettings = serde_json::from_str(json).unwrap();
        assert!(s.storage_repair);
    }

    #[test]
    fn settings_default_includes_storage_repair() {
        assert!(NotificationSettings::default().storage_repair);
    }
}

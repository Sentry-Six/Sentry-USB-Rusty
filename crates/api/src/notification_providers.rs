//! Scoped notification configuration updates preserve unrelated setup values.
use std::{collections::HashMap, sync::Mutex};
use axum::{http::{header, StatusCode}, response::{IntoResponse, Response}, Json};
use serde::Deserialize;

pub(super) static PROVIDER_CONFIG_LOCK: Mutex<()> = Mutex::new(());
const PROVIDERS: &[(&str, &[&str], &[&str])] = &[
    ("PUSHOVER_ENABLED", &["PUSHOVER_USER_KEY", "PUSHOVER_APP_KEY"], &[]),
    ("GOTIFY_ENABLED", &["GOTIFY_DOMAIN", "GOTIFY_APP_TOKEN"], &["GOTIFY_PRIORITY"]),
    ("DISCORD_ENABLED", &["DISCORD_WEBHOOK_URL"], &[]),
    ("TELEGRAM_ENABLED", &["TELEGRAM_CHAT_ID", "TELEGRAM_BOT_TOKEN"], &[]),
    ("IFTTT_ENABLED", &["IFTTT_EVENT_NAME", "IFTTT_KEY"], &[]),
    ("SLACK_ENABLED", &["SLACK_WEBHOOK_URL"], &[]),
    ("SIGNAL_ENABLED", &["SIGNAL_URL", "SIGNAL_FROM_NUM", "SIGNAL_TO_NUM"], &[]),
    ("MATRIX_ENABLED", &["MATRIX_SERVER_URL", "MATRIX_USERNAME", "MATRIX_PASSWORD", "MATRIX_ROOM"], &[]),
    ("SNS_ENABLED", &["AWS_REGION", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SNS_TOPIC_ARN"], &[]),
    ("WEBHOOK_ENABLED", &["WEBHOOK_URL"], &[]),
    ("NTFY_ENABLED", &["NTFY_URL"], &["NTFY_TOKEN", "NTFY_PRIORITY"]),
    ("MOBILE_PUSH_ENABLED", &[], &[]),
];
fn allowed(key: &str) -> bool {
    key == "NOTIFICATION_TITLE" || PROVIDERS.iter().any(|(enabled, required, optional)|
        key == *enabled || required.contains(&key) || optional.contains(&key))
}
fn scoped(active: &HashMap<String, String>) -> HashMap<String, String> {
    active.iter().filter(|(key, _)| allowed(key)).map(|(k,v)| (k.clone(),v.clone())).collect()
}
#[derive(Deserialize)]
pub struct ProviderUpdate {
    changes: HashMap<String, String>,
    expected: HashMap<String, String>,
}
fn patch(active: &mut HashMap<String, String>, request: &ProviderUpdate) -> Result<(), (StatusCode, &'static str)> {
    if request.changes.keys().chain(request.expected.keys()).any(|key| !allowed(key)) {
        return Err((StatusCode::BAD_REQUEST, "Unsupported notification setting"));
    }
    if scoped(active) != request.expected {
        return Err((StatusCode::CONFLICT, "Notification settings changed elsewhere. Reload before saving."));
    }
    if request.changes.values().any(|value| value.contains(['\n', '\r'])) {
        return Err((StatusCode::BAD_REQUEST, "Notification settings must use single-line values"));
    }
    let mut next = active.clone();
    for (key, value) in &request.changes { next.insert(key.clone(), value.clone()); }
    for (enabled, required, optional) in PROVIDERS {
        let changed = request.changes.contains_key(*enabled) || required.iter().chain(optional.iter()).any(|key| request.changes.contains_key(*key));
        if !changed { continue; }
        if required.is_empty() {
            if !matches!(next.get(*enabled).map(String::as_str), Some("true" | "false")) {
                return Err((StatusCode::BAD_REQUEST, "Invalid notification enable value"));
            }
            continue;
        }
        let any = required.iter().any(|key| next.get(*key).is_some_and(|v| !v.trim().is_empty()));
        let all = required.iter().all(|key| next.get(*key).is_some_and(|v| !v.trim().is_empty()));
        if any && !all { return Err((StatusCode::BAD_REQUEST, "Complete all required fields for each configured provider")); }
        next.insert(enabled.to_string(), any.to_string());
    }
    *active = next;
    Ok(())
}

pub async fn get_provider_config() -> Response {
    match tokio::task::spawn_blocking(|| {
        let _guard = PROVIDER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        sentryusb_config::parse_file(sentryusb_config::find_config_path()).map(|(active, _)| scoped(&active))
    }).await {
        Ok(Ok(values)) => ([(header::CACHE_CONTROL, "no-store")], Json(serde_json::json!({ "values": values }))).into_response(),
        _ => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not load notification providers").into_response(),
    }
}

pub async fn save_provider_config(Json(request): Json<ProviderUpdate>) -> Response {
    // No setup run or service restart: notification dispatch reads config for every send.
    let result = tokio::task::spawn_blocking(move || {
        let _guard = PROVIDER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = sentryusb_config::find_config_path();
        let (mut active, _) = sentryusb_config::parse_file(path)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not load notification providers"))?;
        patch(&mut active, &request)?;
        let _ = std::process::Command::new("/root/bin/remountfs_rw").status();
        sentryusb_config::write_file(path, &active)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Could not save notification providers"))?;
        Ok::<_, (StatusCode, &'static str)>(scoped(&active))
    }).await;
    match result {
        Ok(Ok(values)) => ([(header::CACHE_CONTROL, "no-store")], Json(serde_json::json!({"values": values}))).into_response(),
        Ok(Err((status, message))) => crate::json_error(status, message).into_response(),
        Err(_) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save notification providers").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_patch_preserves_unrelated_and_disabled_values() {
        let mut active = HashMap::from([
            ("SNAPSHOT_INTERVAL".into(), "480".into()),
            ("WEB_PASSWORD".into(), "unchanged".into()),
            ("PUSHOVER_ENABLED".into(), "false".into()),
            ("PUSHOVER_USER_KEY".into(), "stored".into()),
        ]);
        let expected = scoped(&active);
        patch(&mut active, &ProviderUpdate { expected, changes: HashMap::from([
            ("TELEGRAM_CHAT_ID".into(), "123".into()), ("TELEGRAM_BOT_TOKEN".into(), "token".into()),
        ]) }).unwrap();
        assert_eq!(active["SNAPSHOT_INTERVAL"], "480");
        assert_eq!(active["WEB_PASSWORD"], "unchanged");
        assert_eq!(active["PUSHOVER_ENABLED"], "false");
        assert_eq!(active["TELEGRAM_ENABLED"], "true");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "export SNAPSHOT_INTERVAL=480\nexport WEB_PASSWORD=unchanged\n# preserved comment\n").unwrap();
        sentryusb_config::write_file(path.to_str().unwrap(), &active).unwrap();
        let (saved, _) = sentryusb_config::parse_file(path.to_str().unwrap()).unwrap();
        assert_eq!(saved, active);
        assert!(std::fs::read_to_string(path).unwrap().contains("# preserved comment"));
    }
    #[test]
    fn provider_patch_rejects_stale_unknown_and_partial_changes_without_mutating() {
        let original = HashMap::from([("WEB_PASSWORD".into(), "unchanged".into())]);
        for request in [
            ProviderUpdate { expected: HashMap::new(), changes: HashMap::from([("WEB_PASSWORD".into(), "changed".into())]) },
            ProviderUpdate { expected: HashMap::new(), changes: HashMap::from([("TELEGRAM_CHAT_ID".into(), "123".into())]) },
            ProviderUpdate { expected: HashMap::from([("NOTIFICATION_TITLE".into(), "old".into())]), changes: HashMap::new() },
        ] {
            let mut active = original.clone();
            assert!(patch(&mut active, &request).is_err());
            assert_eq!(active, original);
        }
    }
}

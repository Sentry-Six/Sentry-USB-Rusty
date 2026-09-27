//! Native notification providers for SentryUSB.
//!
//! Replaces the bash `send-push-message` script with direct HTTP calls,
//! eliminating subprocess overhead and Python/curl dependencies.

use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn};

pub mod discord;
pub mod gotify;
pub mod ifttt;
pub mod matrix;
pub mod ntfy;
pub mod pushover;
pub mod sentry_connect;
pub mod signal;
pub mod slack;
pub mod sns;
pub mod telegram;
pub mod webhook;

/// Trait for notification providers.
#[async_trait]
pub trait NotificationProvider: Send + Sync {
    /// Send a notification with the given title and message.
    async fn send(&self, title: &str, message: &str) -> Result<()>;
    /// Provider name for logging/display.
    fn name(&self) -> &str;
}

/// Configuration for all notification providers, read from sentryusb.conf.
#[derive(Default)]
pub struct NotifyConfig {
    pub pushover_enabled: bool,
    pub pushover_app_key: String,
    pub pushover_user_key: String,

    pub discord_enabled: bool,
    pub discord_webhook_url: String,

    pub telegram_enabled: bool,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub telegram_silent: bool,

    pub slack_enabled: bool,
    pub slack_webhook_url: String,

    pub gotify_enabled: bool,
    pub gotify_domain: String,
    pub gotify_app_token: String,
    pub gotify_priority: String,

    pub ntfy_enabled: bool,
    pub ntfy_url: String,
    pub ntfy_token: String,
    pub ntfy_priority: String,

    pub ifttt_enabled: bool,
    pub ifttt_event_name: String,
    pub ifttt_key: String,

    pub webhook_enabled: bool,
    pub webhook_url: String,

    pub signal_enabled: bool,
    pub signal_url: String,
    pub signal_from_num: String,
    pub signal_to_num: String,

    pub matrix_enabled: bool,
    pub matrix_server_url: String,
    pub matrix_username: String,
    pub matrix_password: String,
    pub matrix_room: String,

    pub sns_enabled: bool,
    pub sns_topic_arn: String,
    pub sns_region: String,
    // Passed to the SNS signer because systemd starts the server without
    // sourcing sentryusb.conf — env-only AWS credential lookups never
    // resolve on a normal install (see sns.rs).
    pub sns_access_key: String,
    pub sns_secret_key: String,

    pub mobile_push_enabled: bool,
    pub mobile_push_device_id: String,
    pub mobile_push_secret: String,
}

impl NotifyConfig {
    /// Load notification config from sentryusb.conf.
    pub fn from_config() -> Self {
        let config_path = sentryusb_config::find_config_path();
        let (active, _) = sentryusb_config::parse_file(config_path)
            .unwrap_or_default();

        let get = |key: &str| -> String {
            active.get(key).cloned().unwrap_or_default()
        };
        let is_true = |key: &str| -> bool {
            get(key).to_lowercase() == "true"
        };

        let mobile_push_enabled = is_true("MOBILE_PUSH_ENABLED");

        // MOBILE_PUSH_DEVICE_ID and MOBILE_PUSH_SECRET are intentionally NOT
        // stored in sentryusb.conf — they live in the credentials JSON managed
        // by the server. envsetup.sh reads them from that file;
        // we do the same here when the conf values are absent.
        let (mobile_push_device_id, mobile_push_secret) = {
            let from_conf = (get("MOBILE_PUSH_DEVICE_ID"), get("MOBILE_PUSH_SECRET"));
            if !from_conf.0.is_empty() && !from_conf.1.is_empty() {
                from_conf
            } else {
                read_mobile_credentials_from_json()
                    .unwrap_or_else(|| (String::new(), String::new()))
            }
        };

        NotifyConfig {
            pushover_enabled: is_true("PUSHOVER_ENABLED"),
            pushover_app_key: get("PUSHOVER_APP_KEY"),
            pushover_user_key: get("PUSHOVER_USER_KEY"),

            discord_enabled: is_true("DISCORD_ENABLED"),
            discord_webhook_url: get("DISCORD_WEBHOOK_URL"),

            telegram_enabled: is_true("TELEGRAM_ENABLED"),
            telegram_bot_token: get("TELEGRAM_BOT_TOKEN"),
            telegram_chat_id: get("TELEGRAM_CHAT_ID"),
            telegram_silent: get("TELEGRAM_SILENT_NOTIFY").to_lowercase() == "true",

            slack_enabled: is_true("SLACK_ENABLED"),
            slack_webhook_url: get("SLACK_WEBHOOK_URL"),

            gotify_enabled: is_true("GOTIFY_ENABLED"),
            gotify_domain: get("GOTIFY_DOMAIN"),
            gotify_app_token: get("GOTIFY_APP_TOKEN"),
            gotify_priority: get("GOTIFY_PRIORITY"),

            ntfy_enabled: is_true("NTFY_ENABLED"),
            ntfy_url: get("NTFY_URL"),
            ntfy_token: get("NTFY_TOKEN"),
            ntfy_priority: get("NTFY_PRIORITY"),

            ifttt_enabled: is_true("IFTTT_ENABLED"),
            ifttt_event_name: get("IFTTT_EVENT_NAME"),
            ifttt_key: get("IFTTT_KEY"),

            webhook_enabled: is_true("WEBHOOK_ENABLED"),
            webhook_url: get("WEBHOOK_URL"),

            signal_enabled: is_true("SIGNAL_ENABLED"),
            signal_url: get("SIGNAL_URL"),
            signal_from_num: get("SIGNAL_FROM_NUM"),
            signal_to_num: get("SIGNAL_TO_NUM"),

            matrix_enabled: is_true("MATRIX_ENABLED"),
            matrix_server_url: get("MATRIX_SERVER_URL"),
            matrix_username: get("MATRIX_USERNAME"),
            matrix_password: get("MATRIX_PASSWORD"),
            matrix_room: get("MATRIX_ROOM"),

            sns_enabled: is_true("SNS_ENABLED"),
            sns_topic_arn: get("AWS_SNS_TOPIC_ARN"),
            sns_region: get("AWS_REGION"),
            sns_access_key: get("AWS_ACCESS_KEY_ID"),
            sns_secret_key: get("AWS_SECRET_ACCESS_KEY"),

            mobile_push_enabled,
            mobile_push_device_id,
            mobile_push_secret,
        }
    }
}

/// Read device_id and device_secret from the credentials JSON file.
/// Mirrors envsetup.sh's fallback for mobile push credentials.
fn read_mobile_credentials_from_json() -> Option<(String, String)> {
    const CREDS_PATH: &str = "/root/.sentryusb/notification-credentials.json";
    let data = std::fs::read_to_string(CREDS_PATH).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let id = v.get("device_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let secret = v.get("device_secret").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    Some((id, secret))
}

/// Request-level context for a single notification dispatch. Only
/// mobile push (Sentry Connect) currently consumes the extras — the
/// other channels are title + message only.
#[derive(Debug, Clone, Default)]
pub struct NotifyRequest<'a> {
    pub title: &'a str,
    pub message: &'a str,
    /// `start` / `finish` — matches the bash `$3` positional arg. Drives
    /// the live_activity branch on mobile push.
    pub type_hint: Option<&'a str>,
    /// Notification category (`archive_start`, `temperature`, `drives`,
    /// …). Echoed to the mobile push server for categorization and used
    /// for the gate check upstream.
    pub notification_type: Option<&'a str>,
    /// Total clip count for the pending archive run — enables the
    /// live_activity payload on `archive_start`.
    pub archive_total_count: Option<u32>,
}

/// Process-wide shared client for outbound notification dispatches.
/// Built once on first send; reused for the lifetime of the process so
/// we don't repeatedly stand up a fresh TLS pool + DNS cache + idle
/// connection pool per sentry event.
static NOTIFY_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn notify_client() -> &'static reqwest::Client {
    NOTIFY_CLIENT.get_or_init(|| {
        // Panic-on-error rather than fall back to `Client::default()`: the
        // default builder discards our 30s/10s timeouts, so a TLS init
        // failure would silently produce a no-timeout client that can
        // hang a tokio worker indefinitely on a misconfigured push
        // endpoint. Failing loudly at first send surfaces the real
        // problem instead of pretending notifications still work.
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest Client::build failed (TLS init?)")
    })
}

/// Context-aware dispatch — preferred entry point for runtime
/// notifications. Passes the extra context through to providers that
/// can use it (currently just Sentry Connect); others ignore the extras
/// and use title + message.
pub async fn send_to_all_with_context(
    config: &NotifyConfig,
    req: &NotifyRequest<'_>,
) -> Vec<(String, Result<()>)> {
    send_to_selected(config, req, &[]).await
}

/// Retry only providers that have not already confirmed acceptance.
pub async fn send_to_selected(
    config: &NotifyConfig,
    req: &NotifyRequest<'_>,
    delivered: &[String],
) -> Vec<(String, Result<()>)> {
    use futures::future::BoxFuture;

    let client = notify_client();

    let title = req.title;
    let message = req.message;

    // Fan the providers out CONCURRENTLY. Each send is an independent
    // HTTP call with its own 30s timeout; dispatching sequentially made
    // total latency the SUM of every enabled channel — a time-sensitive
    // Sentry alert's mobile push could sit behind a slow or unreachable
    // Discord/Gotify endpoint for up to 30s per channel. Concurrent
    // dispatch makes it the MAX instead, with per-provider results (and
    // ordering) unchanged for callers.
    let mut sends: Vec<(&'static str, String, BoxFuture<'_, Result<()>>)> = Vec::new();

    if config.pushover_enabled {
        sends.push(("Pushover", "pushover".into(), Box::pin(
            pushover::send(client, &config.pushover_app_key, &config.pushover_user_key, title, message),
        )));
    }

    if config.discord_enabled {
        sends.push(("Discord", "discord".into(), Box::pin(
            discord::send(client, &config.discord_webhook_url, title, message),
        )));
    }

    if config.telegram_enabled {
        sends.push(("Telegram", "telegram".into(), Box::pin(
            telegram::send(client, &config.telegram_bot_token, &config.telegram_chat_id, title, message, config.telegram_silent),
        )));
    }

    if config.slack_enabled {
        sends.push(("Slack", "slack".into(), Box::pin(
            slack::send(client, &config.slack_webhook_url, title, message),
        )));
    }

    if config.gotify_enabled {
        sends.push(("Gotify", "gotify".into(), Box::pin(
            gotify::send(client, &config.gotify_domain, &config.gotify_app_token, &config.gotify_priority, title, message),
        )));
    }

    if config.ntfy_enabled {
        sends.push(("ntfy", "ntfy".into(), Box::pin(
            ntfy::send(client, &config.ntfy_url, &config.ntfy_token, &config.ntfy_priority, title, message),
        )));
    }

    if config.ifttt_enabled {
        sends.push(("IFTTT", "ifttt".into(), Box::pin(
            ifttt::send(client, &config.ifttt_event_name, &config.ifttt_key, title, message),
        )));
    }

    if config.webhook_enabled {
        sends.push(("Webhook", "webhook".into(), Box::pin(
            webhook::send(client, &config.webhook_url, title, message),
        )));
    }

    if config.signal_enabled {
        sends.push(("Signal", "signal".into(), Box::pin(
            signal::send(client, &config.signal_url, &config.signal_from_num, &config.signal_to_num, message),
        )));
    }

    if config.matrix_enabled {
        sends.push(("Matrix", "matrix".into(), Box::pin(
            matrix::send(client, &config.matrix_server_url, &config.matrix_username, &config.matrix_password, &config.matrix_room, title, message),
        )));
    }

    if config.mobile_push_enabled {
        let ctx = sentry_connect::SendContext {
            type_hint: req.type_hint,
            notification_type: req.notification_type,
            archive_total_count: req.archive_total_count,
            device_name: Some(title),
        };
        sends.push(("Mobile Push", "mobile_push".into(), Box::pin(async move {
            sentry_connect::send_with_context(
                client,
                &config.mobile_push_device_id,
                &config.mobile_push_secret,
                title,
                message,
                &ctx,
            )
            .await
        })));
    }

    if config.sns_enabled {
        sends.push(("SNS", "sns".into(), Box::pin(
            sns::send(client, &config.sns_topic_arn, &config.sns_region, &config.sns_access_key, &config.sns_secret_key, title, message),
        )));
    }

    futures::future::join_all(sends.into_iter().filter(|(_, key, _)| !delivered.contains(key)).map(
        |(display, key, fut)| async move {
            // Sanitize before either logging or returning errors to API/history.
            let r = fut.await.map_err(|e| anyhow::anyhow!(safe_provider_error(&e, &[
                &config.pushover_app_key, &config.pushover_user_key,
                &config.telegram_bot_token, &config.gotify_app_token, &config.ntfy_token,
                &config.ifttt_key, &config.matrix_password, &config.sns_access_key,
                &config.sns_secret_key, &config.mobile_push_secret,
                &config.discord_webhook_url, &config.slack_webhook_url, &config.webhook_url,
            ])));
            log_result(display, &r);
            (key, r)
        },
    ))
    .await
}

fn log_result(provider: &str, result: &Result<()>) {
    match result {
        Ok(()) => info!("[notify] {} — sent successfully", provider),
        Err(e) => warn!("[notify] {} — failed: {}", provider, e),
    }
}

#[cfg(test)]
mod update_retry_tests {
    use super::*;
    #[tokio::test]
    async fn accepted_provider_is_not_contacted_again_on_retry() {
        let config = NotifyConfig {
            // A delivered provider must not be contacted even if its endpoint
            // is now unavailable. The remaining provider must still be tried.
            webhook_enabled: true, webhook_url: "http://127.0.0.1:1/already-delivered".into(),
            ntfy_enabled: true, ntfy_url: "http://127.0.0.1:1/pending".into(),
            ..Default::default()
        };
        let result = send_to_selected(&config, &NotifyRequest {
            title: "Test", message: "Update complete", notification_type: Some("update"),
            type_hint: Some("info"), archive_total_count: None,
        }, &["webhook".into()]).await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "ntfy");
        assert!(result[0].1.is_err());
    }
}

/// Preserve diagnostic reasons without credentials or secret-bearing URLs.
pub fn safe_provider_error(error: &anyhow::Error, secrets: &[&str]) -> String {
    safe_error_text(&format!("{error:#}"), secrets)
}

/// Redact a credential assignment, not a diagnostic mentioning its field name.
fn redact_credential_values(mut text: String) -> String {
    for key in ["authorization", "x-device-secret", "device_secret", "access_token", "password", "api_key", "token", "secret"] {
        let mut cursor = 0;
        while let Some(found) = text[cursor..].to_ascii_lowercase().find(key) {
            let start = cursor + found;
            let end = start + key.len();
            cursor = end;
            if start > 0 && (text.as_bytes()[start - 1].is_ascii_alphanumeric() || text.as_bytes()[start - 1] == b'_') {
                continue;
            }
            let bytes = text.as_bytes();
            let mut at = end;
            if at < bytes.len() && matches!(bytes[at], b'"' | b'\'') { at += 1; }
            while at < bytes.len() && bytes[at].is_ascii_whitespace() { at += 1; }
            if at >= bytes.len() || !matches!(bytes[at], b':' | b'=') { continue; }
            at += 1;
            while at < bytes.len() && bytes[at].is_ascii_whitespace() { at += 1; }
            if at >= bytes.len() { continue; }
            let from;
            let mut to;
            if matches!(bytes[at], b'"' | b'\'') {
                let quote = bytes[at];
                from = at + 1;
                to = from;
                while to < bytes.len() && bytes[to] != quote {
                    if bytes[to] == b'\\' && to + 1 < bytes.len() { to += 1; }
                    to += 1;
                }
            } else {
                from = at;
                to = at;
                while to < bytes.len() && bytes[to] != b'\n' && bytes[to] != b'\r'
                    && (key == "authorization" || !matches!(bytes[to], b' ' | b'\t' | b',' | b';' | b'}')) {
                    to += 1;
                }
            }
            text.replace_range(from..to, "[redacted]");
            cursor = from + "[redacted]".len();
        }
    }
    text
}

pub fn safe_error_text(message: &str, secrets: &[&str]) -> String {
    let mut safe = message.to_string();
    // Longest first prevents one credential exposing the suffix of another.
    let mut secrets: Vec<&str> = secrets.iter().copied().filter(|s| !s.is_empty()).collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in secrets {
        safe = safe.replace(secret, "[redacted]");
        safe = safe.replace(urlencoding::encode(secret).as_ref(), "[redacted]");
    }
    let safe = safe.lines().map(|line| {
        line.split_whitespace().map(|word| {
            let lower = word.to_ascii_lowercase();
            if lower.contains("https://") || lower.contains("http://") {
                "[redacted URL]"
            } else { word }
        }).collect::<Vec<_>>().join(" ")
    }).collect::<Vec<_>>().join("\n");
    // Remove URLs first: a capability path ending in "token:" must not be
    // mistaken for a credential assignment that consumes the following cause.
    let safe = redact_credential_values(safe);
    // A provider can return an HTML error page; keep history bounded.
    if safe.chars().count() > 4096 {
        format!("{}… [truncated]", safe.chars().take(4096).collect::<String>())
    } else { safe }
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn notification_redaction_preserves_missing_field_reason_status_and_cause_chain() {
        assert_eq!(super::safe_error_text("no access_token in login response", &[]),
            "no access_token in login response");
        let safe = super::safe_error_text(
            r#"HTTP 401 — {"error":"invalid device_secret","device_secret":"unknown-secret","access_token":"dynamic-token"}"#, &[]);
        assert!(safe.contains("HTTP 401"));
        assert!(safe.contains("invalid device_secret"));
        assert!(!safe.contains("unknown-secret"));
        assert!(!safe.contains("dynamic-token"));
        let error = anyhow::anyhow!("connection refused").context("error sending request for url https://example/private-token");
        let safe = super::safe_provider_error(&error, &[]);
        assert!(safe.contains("connection refused"));
        assert!(!safe.contains("private-token"));
    }

    #[test]
    fn notification_errors_keep_reasons_without_credentials_or_urls() {
        let error = "HTTP 400 chat not found at https://api.example/botSECRET/send?token=SECRET\nAuthorization: Bearer SECRET\nx-device-secret: SECRET\nrequest timed out";
        let safe = super::safe_error_text(error, &["SECRET"]);
        assert!(safe.contains("HTTP 400 chat not found"));
        assert!(safe.contains("request timed out"));
        assert!(!safe.contains("SECRET"));
        assert!(!safe.contains("api.example"));
        assert!(!safe.contains("Bearer"));
        assert!(!super::safe_error_text("password=p%40ss", &["p@ss"]).contains("p%40ss"));
        assert!(!super::safe_error_text("HTTP 400 HTTPS://example/private-capability", &[]).contains("private-capability"));
    }
}

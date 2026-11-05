use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

/// Whole-request budget for small requests, and the base for larger bodies.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Extra upload time per MiB of request body: about 50 KiB/s, a weak
/// cellular uplink, still finishes a full 14 MiB route batch in time.
const TIMEOUT_PER_MIB: Duration = Duration::from_secs(20);
/// No single request may hold a sweep for longer than this.
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Total time allowed for a request carrying `body_bytes`: the base budget
/// plus time per MiB, capped. Small requests keep the old 30 s limit.
pub fn request_timeout(body_bytes: usize) -> Duration {
    let extra_ms = (body_bytes as u128 * TIMEOUT_PER_MIB.as_millis()) / (1024 * 1024);
    let extra = Duration::from_millis(u64::try_from(extra_ms).unwrap_or(u64::MAX));
    REQUEST_TIMEOUT.saturating_add(extra).min(MAX_REQUEST_TIMEOUT)
}

/// What a non-success Cloud reply means for the stored pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// JSON 401 `unknown_token` or 403 `revoked`: the Cloud no longer
    /// accepts this Pi, so the local pairing is removed.
    Revoked,
    /// JSON 403 `user_suspended`: keep the pairing and pause uploads.
    UserSuspended,
    /// Anything else, including HTML, empty or unrecognised 401/403 replies
    /// from a proxy or CDN. The pairing is kept and the request retried later.
    Temporary,
}

/// Classifies a reply by status and body. Only the two explicit revoke
/// replies may remove credentials; a block page must never unpair a Pi.
pub fn classify_rejection(status: u16, body: &str) -> Rejection {
    let error = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned));
    match (status, error.as_deref()) {
        (401, Some("unknown_token")) | (403, Some("revoked")) => Rejection::Revoked,
        (403, Some("user_suspended")) => Rejection::UserSuspended,
        _ => Rejection::Temporary,
    }
}

/// A short, single-line excerpt of a reply body for logs.
pub fn body_excerpt(body: &str) -> String {
    let flat: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(160)
        .collect();
    if body.chars().count() > 160 { format!("{flat}...") } else { flat }
}

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("cloud rejected with HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Only for an explicit revoke reply (see `classify_rejection`).
    #[error("auth rejected; pi credentials should be wiped")]
    AuthRejected,

    #[error("user suspended; uploads paused until reinstated")]
    UserSuspended,

    #[error("pi key stale; rekey required before upload retry")]
    PiKeyStale,

    #[error("response parse: {0}")]
    Parse(#[from] serde_json::Error),
}

pub struct CloudClient {
    inner: reqwest::Client,
    base_url: String,
    bearer: Option<String>,
}

impl CloudClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let inner = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");
        CloudClient {
            inner,
            base_url: base_url.into(),
            bearer: None,
        }
    }

    pub fn with_bearer(mut self, token_bytes: &[u8]) -> Self {
        self.bearer = Some(B64.encode(token_bytes));
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    pub async fn post_json_anon(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, CloudError> {
        let resp = self
            .inner
            .post(self.url(path))
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }

    pub async fn get_with_header(
        &self,
        path: &str,
        header: (&str, &str),
    ) -> Result<reqwest::Response, CloudError> {
        let resp = self
            .inner
            .get(self.url(path))
            .header(header.0, header.1)
            .send()
            .await?;
        Ok(resp)
    }

    pub async fn get_bearer(&self, path: &str) -> Result<reqwest::Response, CloudError> {
        let bearer = self
            .bearer
            .as_deref()
            .ok_or_else(|| CloudError::Http { status: 0, body: "no bearer".into() })?;
        let resp = self
            .inner
            .get(self.url(path))
            .header("Authorization", format!("Bearer {}", bearer))
            .send()
            .await?;
        Ok(resp)
    }

    pub async fn post_json_bearer(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, CloudError> {
        self.post_json_bearer_with_headers(path, body, &[]).await
    }

    pub async fn post_json_bearer_with_headers(
        &self,
        path: &str,
        body: &impl Serialize,
        extra_headers: &[(&str, String)],
    ) -> Result<reqwest::Response, CloudError> {
        let bearer = self
            .bearer
            .as_deref()
            .ok_or_else(|| CloudError::Http { status: 0, body: "no bearer".into() })?;
        // Serialize first so the time budget can follow the body size.
        let bytes = serde_json::to_vec(body)?;
        let mut req = self
            .inner
            .post(self.url(path))
            .header("Authorization", format!("Bearer {}", bearer))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(request_timeout(bytes.len()));
        for (k, v) in extra_headers {
            req = req.header(*k, v.clone());
        }
        let resp = req.body(bytes).send().await?;
        Ok(resp)
    }

    pub async fn classify(resp: reqwest::Response) -> Result<reqwest::Response, CloudError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        let body_text = resp.text().await.unwrap_or_default();
        let body_json: Option<Value> = serde_json::from_str(&body_text).ok();

        match classify_rejection(status.as_u16(), &body_text) {
            Rejection::Revoked => return Err(CloudError::AuthRejected),
            Rejection::UserSuspended => return Err(CloudError::UserSuspended),
            Rejection::Temporary => {}
        }

        if status.as_u16() == 409
            && body_json
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|e| e.as_str())
                == Some("pi_key_stale")
        {
            return Err(CloudError::PiKeyStale);
        }

        warn!("cloud rejected HTTP {} body={}", status, body_text);
        Err(CloudError::Http {
            status: status.as_u16(),
            body: body_text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_revoke_replies_remove_the_pairing() {
        let html = "<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title></head></html>";
        for (status, body, expected) in [
            (401, r#"{"error":"unknown_token"}"#, Rejection::Revoked),
            (403, r#"{"error":"revoked"}"#, Rejection::Revoked),
            (403, r#"{"error":"user_suspended"}"#, Rejection::UserSuspended),
            (403, html, Rejection::Temporary),
            (403, "", Rejection::Temporary),
            (403, r#"{"error":"forbidden"}"#, Rejection::Temporary),
            (403, r#"{"error":"pi_sync_unavailable"}"#, Rejection::Temporary),
            (403, r#"{"message":"revoked"}"#, Rejection::Temporary),
            (403, r#"{"error":{"code":"revoked"}}"#, Rejection::Temporary),
            (403, r#"{"error":"unknown_token"}"#, Rejection::Temporary),
            (401, r#"{"error":"revoked"}"#, Rejection::Temporary),
            (401, r#"{"error":"missing_token"}"#, Rejection::Temporary),
            (401, html, Rejection::Temporary),
            (401, "", Rejection::Temporary),
            (500, r#"{"error":"revoked"}"#, Rejection::Temporary),
            (502, html, Rejection::Temporary),
            (503, r#"{"error":"unknown_token"}"#, Rejection::Temporary),
        ] {
            assert_eq!(classify_rejection(status, body), expected, "{status} {body}");
        }
    }

    #[test]
    fn request_timeout_grows_with_the_body_and_is_capped() {
        assert_eq!(request_timeout(0), Duration::from_secs(30));
        assert_eq!(request_timeout(4 * 1024), Duration::from_millis(30_078));
        assert_eq!(request_timeout(1024 * 1024), Duration::from_secs(50));
        // The largest route batch the uploader builds.
        assert_eq!(request_timeout(14 * 1024 * 1024), Duration::from_secs(310));
        assert_eq!(request_timeout(64 * 1024 * 1024), MAX_REQUEST_TIMEOUT);
        assert_eq!(request_timeout(usize::MAX), MAX_REQUEST_TIMEOUT);
        let sizes = [0, 1, 512 * 1024, 3 * 1024 * 1024, 14 * 1024 * 1024, 40 * 1024 * 1024];
        assert!(sizes.windows(2).all(|w| request_timeout(w[0]) <= request_timeout(w[1])));
    }

    #[test]
    fn body_excerpts_are_short_single_lines() {
        assert_eq!(body_excerpt("a\nb"), "a b");
        let long = "x".repeat(400);
        assert_eq!(body_excerpt(&long).len(), 163);
    }
}

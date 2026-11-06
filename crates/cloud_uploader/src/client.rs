use std::sync::atomic::{AtomicBool, Ordering};
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

/// Most of a 401/403 body kept for classification and for the caller.
const MAX_REJECTION_BODY: usize = 64 * 1024;

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
    /// Set when any authenticated request got an explicit revoke reply.
    revoked: AtomicBool,
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
            revoked: AtomicBool::new(false),
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
        self.observe(resp).await
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
        self.observe(resp).await
    }

    /// True once any authenticated request from this client got an explicit
    /// revoke reply, whichever endpoint sent it. Callers that do not inspect
    /// 401/403 themselves use this to reach `handle_remote_revoke`.
    pub fn remote_revoke_seen(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }

    /// Notes explicit revoke replies, then hands back an equivalent response
    /// so callers can still read the status, headers and body.
    async fn observe(&self, mut resp: reqwest::Response) -> Result<reqwest::Response, CloudError> {
        let status = resp.status();
        if !matches!(status.as_u16(), 401 | 403) {
            return Ok(resp);
        }
        let version = resp.version();
        let mut headers = resp.headers().clone();
        // The body below is already decoded; its length is the new body's.
        headers.remove(reqwest::header::CONTENT_LENGTH);
        // Error replies are small; a larger page is cut off, not buffered.
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            let room = MAX_REJECTION_BODY - body.len();
            body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if body.len() == MAX_REJECTION_BODY {
                break;
            }
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        match classify_rejection(status.as_u16(), &text) {
            Rejection::Revoked => self.revoked.store(true, Ordering::SeqCst),
            Rejection::UserSuspended => {}
            Rejection::Temporary => warn!(
                "cloud request: HTTP {} is not a revoke; keeping pairing ({})",
                status.as_u16(),
                body_excerpt(&text)
            ),
        }
        let mut rebuilt = http::Response::new(body);
        *rebuilt.status_mut() = status;
        *rebuilt.version_mut() = version;
        *rebuilt.headers_mut() = headers;
        Ok(reqwest::Response::from(rebuilt))
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

    /// Answers each request in turn with a raw status line, content type and body.
    async fn raw_server(replies: Vec<(u16, &'static str, String)>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for (status, content_type, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = headers.lines()
                            .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length { break; }
                    }
                }
                let head = format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
            }
        });
        (base, task)
    }

    #[tokio::test]
    async fn any_authenticated_endpoint_can_report_a_revoke_and_callers_still_read_the_reply() {
        let json = "application/json";
        let html = "<!DOCTYPE html><html><title>Attention Required! | Cloudflare</title></html>";
        for (status, content_type, body, revoked) in [
            (401, json, r#"{"error":"unknown_token"}"#, true),
            (403, json, r#"{"error":"revoked"}"#, true),
            (403, json, r#"{"error":"user_suspended"}"#, false),
            (403, "text/html", html, false),
            (403, "text/plain", "", false),
            (403, json, r#"{"error":"pi_sync_unavailable"}"#, false),
            (500, json, r#"{"error":"revoked"}"#, false),
            (200, json, r#"{"ok":true}"#, false),
        ] {
            let (base, task) = raw_server(vec![
                (status, content_type, body.to_string()),
                (status, content_type, body.to_string()),
            ]).await;
            let client = CloudClient::new(&base).with_bearer(&[3; 32]);
            let response = client.post_json_bearer("/api/pi/sync/state", &serde_json::json!({"piId":"pi"})).await.unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.text().await.unwrap(), body);
            assert_eq!(client.remote_revoke_seen(), revoked, "{status} {body}");
            // GET feeds report it the same way.
            let fresh = CloudClient::new(&base).with_bearer(&[3; 32]);
            let response = fresh.get_bearer("/api/pi/sync/mutables").await.unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(fresh.remote_revoke_seen(), revoked, "GET {status} {body}");
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn classify_keeps_the_pairing_for_proxy_pages() {
        let html = "<html><body>blocked</body></html>";
        for (status, content_type, body, auth_rejected) in [
            (401, "application/json", r#"{"error":"unknown_token"}"#, true),
            (403, "application/json", r#"{"error":"revoked"}"#, true),
            (403, "text/html", html, false),
            (401, "text/html", html, false),
            (403, "application/json", r#"{"error":"nope"}"#, false),
        ] {
            let (base, task) = raw_server(vec![(status, content_type, body.to_string())]).await;
            let client = CloudClient::new(&base);
            let response = client.post_json_anon("/api/pi/pair/handshake", &serde_json::json!({})).await.unwrap();
            let error = CloudClient::classify(response).await.unwrap_err();
            assert_eq!(matches!(error, CloudError::AuthRejected), auth_rejected, "{status} {body}");
            task.await.unwrap();
        }
    }

    #[test]
    fn body_excerpts_are_short_single_lines() {
        assert_eq!(body_excerpt("a\nb"), "a b");
        let long = "x".repeat(400);
        assert_eq!(body_excerpt(&long).len(), 163);
    }
}

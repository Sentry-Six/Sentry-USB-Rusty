//! Optional reports of the version this process is actually running.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

const ENDPOINT: &str = "https://api.sentry-six.com/sentryusb-rusty/telemetry";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
static WAKE: OnceLock<mpsc::Sender<()>> = OnceLock::new();

/// Call before startup migration or serving requests can install another version.
pub fn spawn() {
    let version = running_version(option_env!("SENTRYUSB_RELEASE_VERSION"), &crate::update::read_current_version());
    let (tx, rx) = mpsc::channel(1);
    if WAKE.set(tx).is_err() { return; }
    if let Some(version) = version {
        tokio::spawn(run(version, rx));
    }
}

pub(crate) fn nudge() {
    if let Some(tx) = WAKE.get() { let _ = tx.try_send(()); }
}

fn running_version(compiled: Option<&str>, installed: &str) -> Option<String> {
    [compiled.unwrap_or_default(), installed].into_iter().map(str::trim)
        .find(|version| !version.is_empty() && *version != "0.1.0" && *version != "v0.1.0"
            && crate::update::parse_semver(version).is_some())
        .map(str::to_owned)
}

fn opted_in() -> bool {
    crate::preferences::analytics_opted_in()
}

#[derive(Clone, Serialize)]
struct Observation {
    report_kind: &'static str,
    fingerprint: String,
    current_version: String,
}

struct Schedule {
    enabled: bool,
    next: Instant,
    failures: usize,
    pending: Option<Observation>,
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self { enabled: false, next: now, failures: 0, pending: None }
    }

    fn consent(&mut self, enabled: bool, now: Instant) {
        if enabled != self.enabled {
            self.enabled = enabled;
            self.next = now;
            self.failures = 0;
            self.pending = None;
        }
    }

    fn observation(&mut self, now: Instant, version: &str, fingerprint: &str) -> Option<Observation> {
        if !self.enabled || now < self.next || fingerprint.is_empty() { return None; }
        Some(self.pending.get_or_insert_with(|| Observation {
            report_kind: "running_version", fingerprint: fingerprint.into(),
            current_version: version.into(),
        }).clone())
    }

    fn finish(&mut self, success: bool, now: Instant, jitter: u64) {
        if success {
            self.pending = None;
            self.failures = 0;
            self.next = now + DAY;
        } else {
            let seconds = [30, 120, 600, 1800, 3600][self.failures.min(4)];
            self.failures = self.failures.saturating_add(1);
            self.next = now + Duration::from_secs(seconds + jitter % (seconds / 5 + 1));
        }
    }
}

async fn run(version: String, mut wake: mpsc::Receiver<()>) {
    let mut schedule = Schedule::new(Instant::now());
    loop {
        let now = Instant::now();
        schedule.consent(opted_in(), now);
        // Read the identifier only after consent, and never send a partial identity.
        if schedule.enabled {
            if let Some(observation) = schedule.observation(now, &version,
                crate::update::get_fingerprint()) {
                let result = tokio::select! {
                    result = send(&observation) => Some(result),
                    signal = wake.recv() => {
                        if signal.is_none() { return; }
                        None
                    }
                };
                if let Some(success) = result {
                    schedule.finish(success, Instant::now(), rand::random());
                }
                continue;
            }
        }
        // Local preference polling also catches changes made outside the web UI.
        let next = if schedule.enabled && schedule.next > now {
            schedule.next.min(now + Duration::from_secs(60))
        } else { now + Duration::from_secs(60) };
        tokio::select! {
            _ = tokio::time::sleep_until(next) => {},
            signal = wake.recv() => { if signal.is_none() { return; } }
        }
    }
}

async fn send(observation: &Observation) -> bool {
    send_to(observation, ENDPOINT, opted_in).await
}

async fn send_to(observation: &Observation, endpoint: &str, consent: impl FnOnce() -> bool) -> bool {
    if !consent() { return false; }
    match crate::http_client().post(endpoint).timeout(Duration::from_secs(10)).json(observation).send().await {
        Ok(response) if response.status().is_success() => {
            let acknowledged = response.json::<serde_json::Value>().await.ok()
                .and_then(|value| value.get("success").and_then(|v| v.as_bool())) == Some(true);
            if !acknowledged { tracing::warn!("[device-report] response did not confirm acceptance"); }
            acknowledged
        },
        Ok(response) => { tracing::warn!("[device-report] rejected: {}", response.status()); false },
        Err(_) => { tracing::warn!("[device-report] unavailable; retry scheduled"); false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_version_wins_over_a_staged_update() {
        assert_eq!(running_version(Some("v3.24.0"), "v3.25.0").as_deref(), Some("v3.24.0"));
        assert_eq!(running_version(None, "v3.24.0").as_deref(), Some("v3.24.0"));
        assert_eq!(running_version(None, "0.1.0"), None);
        assert_eq!(running_version(Some(""), "dev"), None);
    }

    #[test]
    fn opt_out_never_creates_an_observation_and_clears_a_pending_retry() {
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        assert!(schedule.observation(now, "v3.24.0", "device").is_none());
        schedule.consent(true, now);
        assert!(schedule.observation(now, "v3.24.0", "device").is_some());
        schedule.consent(false, now);
        assert!(schedule.pending.is_none());
        assert!(schedule.observation(now + DAY, "v3.24.0", "device").is_none());
    }

    #[test]
    fn retries_preserve_running_version_and_back_off_with_a_bound() {
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        schedule.consent(true, now);
        let first = schedule.observation(now, "v3.24.0", "device").unwrap();
        schedule.finish(false, now, 0);
        assert!(schedule.observation(now + Duration::from_secs(29), "v3.24.0", "device").is_none());
        let retry = schedule.observation(now + Duration::from_secs(30), "v9.99.0", "device").unwrap();
        assert_eq!(serde_json::to_value(first).unwrap(), serde_json::to_value(&retry).unwrap());
        assert_eq!(retry.current_version, "v3.24.0");
        for _ in 0..100 { schedule.finish(false, now, u64::MAX); }
        assert!(schedule.next <= now + Duration::from_secs(4320));
    }

    #[test]
    fn one_opt_in_report_then_daily_observations_without_update_checks() {
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        schedule.consent(true, now);
        schedule.observation(now, "v3.24.0", "device").unwrap();
        schedule.finish(true, now, 0);
        schedule.consent(true, now + Duration::from_secs(1));
        assert!(schedule.observation(now + Duration::from_secs(1), "v3.24.0", "device").is_none());
        assert!(schedule.observation(now + DAY - Duration::from_secs(1), "v3.24.0", "device").is_none());
        let daily = schedule.observation(now + DAY, "v3.24.0", "device").unwrap();
        assert_eq!(daily.current_version, "v3.24.0");
        let mut restarted = Schedule::new(now);
        restarted.consent(true, now);
        let update = restarted.observation(now, "v3.25.0", "device").unwrap();
        assert_eq!(update.current_version, "v3.25.0");
        assert_eq!(daily.fingerprint, update.fingerprint);
    }

    #[test]
    fn minimal_payload_contains_only_device_identity_running_version_and_protocol_kind() {
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        schedule.consent(true, now);
        assert!(schedule.observation(now, "v3.24.0", "").is_none());
        let observation = schedule.observation(now, "v3.24.0", "device").unwrap();
        let value = serde_json::to_value(observation).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["current_version", "fingerprint", "report_kind"]);
    }

    #[tokio::test]
    async fn transport_sends_nothing_without_consent_and_requires_an_acknowledgement() {
        use axum::{Json, Router, routing::post};
        use std::sync::{Arc, Mutex};
        let received = Arc::new(Mutex::new(Vec::new()));
        let capture = received.clone();
        let app = Router::new().route("/report", post(move |Json(value): Json<serde_json::Value>| {
            let capture = capture.clone();
            async move { capture.lock().unwrap().push(value); Json(serde_json::json!({"success": true})) }
        })).route("/unconfirmed", post(|| async { Json(serde_json::json!({"success": false})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let now = Instant::now();
        let mut schedule = Schedule::new(now);
        schedule.consent(true, now);
        let observation = schedule.observation(now, "v3.24.0", "device").unwrap();
        let endpoint = format!("http://{address}/report");
        assert!(!send_to(&observation, &endpoint, || false).await);
        assert!(received.lock().unwrap().is_empty());
        assert!(send_to(&observation, &endpoint, || true).await);
        assert_eq!(received.lock().unwrap().as_slice(), &[serde_json::to_value(&observation).unwrap()]);
        assert!(!send_to(&observation, &format!("http://{address}/unconfirmed"), || true).await);
        server.abort();
    }

}

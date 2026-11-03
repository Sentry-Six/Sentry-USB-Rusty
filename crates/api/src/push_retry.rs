//! Retry of mobile pushes that failed right after boot.
//!
//! A Pi that powers up away from home sends some alerts (Sentry power
//! requested, archive errors, BLE and temperature warnings) before the
//! hotspot or cellular link is up, so the mobile push fails and was never
//! sent again. This keeps a short, bounded retry so the alert still arrives,
//! without a burst of notifications when the network comes back:
//!
//! * only the types in [`RETRY_TYPES`] are retried, never repeating ones
//!   such as "Update available";
//! * only failures in the first [`BOOT_WINDOW`] after boot are kept, and
//!   retrying stops when that window ends;
//! * only the newest failed push of each type is kept, and a newer push of
//!   the same type that reaches the phone drops the older one (pushes are
//!   ordered by when they were started, not when they finished);
//! * only pushes that never reached the server are retried (no connection),
//!   so a push the server may have accepted is never sent twice;
//! * one push is retried per interval, so queued types do not arrive together;
//! * a push older than [`MAX_AGE`] is dropped instead of sent.
//!
//! Only mobile push is retried; other providers keep their single attempt.
//! The queue is in memory, so it never survives a restart of the server.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Alert types worth delivering late. Everything else is sent once.
pub(crate) const RETRY_TYPES: &[&str] = &["keep_accessory", "archive_error", "keep_awake_failure", "temperature"];
/// Failures later than this after boot are not retried, and retrying stops here.
pub(crate) const BOOT_WINDOW: Duration = Duration::from_secs(8 * 60);
/// A push this old is dropped instead of sent.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(10 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingPush {
    pub title: String,
    pub message: String,
    pub summary: Option<String>,
    pub notification_type: String,
    pub type_hint: Option<String>,
    /// When the original dispatch started; orders pushes of the same type.
    pub started_at: Instant,
}

#[derive(Default)]
pub(crate) struct RetryQueue {
    pending: HashMap<String, PendingPush>,
    /// Start time of the newest push of each type that reached the phone.
    delivered: HashMap<String, Instant>,
    worker_running: bool,
}

impl RetryQueue {
    /// Records a failed mobile push. Returns true when it was kept.
    pub(crate) fn record_failure(&mut self, push: PendingPush, uptime: Duration) -> bool {
        if !RETRY_TYPES.contains(&push.notification_type.as_str()) || uptime >= BOOT_WINDOW {
            return false;
        }
        let kind = push.notification_type.as_str();
        if self.delivered.get(kind).is_some_and(|d| *d >= push.started_at)
            || self.pending.get(kind).is_some_and(|p| p.started_at > push.started_at)
        {
            return false;
        }
        self.pending.insert(push.notification_type.clone(), push);
        true
    }

    /// A push of this type reached the phone: any older one is not needed.
    pub(crate) fn record_delivered(&mut self, notification_type: &str, started_at: Instant) {
        let newest = self.delivered.entry(notification_type.to_string()).or_insert(started_at);
        if *newest < started_at {
            *newest = started_at;
        }
        if self.pending.get(notification_type).is_some_and(|p| p.started_at <= started_at) {
            self.pending.remove(notification_type);
        }
    }

    /// Still the push to send for its type (nothing newer replaced or delivered it).
    pub(crate) fn still_pending(&self, push: &PendingPush) -> bool {
        self.pending.get(&push.notification_type) == Some(push)
    }

    /// Drops what is too old (or everything once the boot window is over) and
    /// returns the oldest push still worth sending. One per call, so queued
    /// types are spread out instead of arriving together.
    pub(crate) fn next_due(&mut self, now: Instant, uptime: Duration) -> Option<PendingPush> {
        if uptime >= BOOT_WINDOW {
            self.pending.clear();
            return None;
        }
        self.pending.retain(|_, p| now.saturating_duration_since(p.started_at) <= MAX_AGE);
        self.pending.values().min_by_key(|p| p.started_at).cloned()
    }

    /// Removes the push once it was sent (or given up), unless a newer one replaced it meanwhile.
    pub(crate) fn sent(&mut self, push: &PendingPush) {
        if self.still_pending(push) {
            self.pending.remove(&push.notification_type);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

static QUEUE: Mutex<Option<RetryQueue>> = Mutex::new(None);

fn with_queue<T>(f: impl FnOnce(&mut RetryQueue) -> T) -> T {
    let mut guard = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(RetryQueue::default))
}

/// Time since the system booted, from `/proc/uptime`. Unknown counts as
/// outside the boot window, so nothing is retried.
pub(crate) fn system_uptime() -> Duration {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::MAX)
}

/// Called after every dispatch with the mobile push outcome (if mobile push
/// ran). `never_sent` is true only when the push cannot have reached the server.
pub(crate) fn after_dispatch(push: PendingPush, mobile_ok: bool, never_sent: bool) {
    if mobile_ok {
        with_queue(|q| q.record_delivered(&push.notification_type, push.started_at));
        return;
    }
    if !never_sent {
        return;
    }
    let kept = with_queue(|q| q.record_failure(push.clone(), system_uptime()));
    if !kept {
        return;
    }
    info!("[notify] mobile push for {} failed after boot; will retry while the network comes up", push.notification_type);
    let start = with_queue(|q| !std::mem::replace(&mut q.worker_running, true));
    if start {
        tokio::spawn(run_worker());
    }
}

async fn run_worker() {
    loop {
        tokio::time::sleep(RETRY_INTERVAL).await;
        let next = with_queue(|q| {
            let next = q.next_due(Instant::now(), system_uptime());
            if next.is_none() {
                q.worker_running = false;
            }
            next
        });
        let Some(push) = next else { return };
        {
            if !crate::notification_center::is_type_enabled(Some(&push.notification_type)) {
                with_queue(|q| q.sent(&push));
                continue;
            }
            let config = sentryusb_notify::NotifyConfig::from_config();
            let message = push.summary.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(&push.message);
            let req = sentryusb_notify::NotifyRequest {
                title: &push.title,
                message,
                type_hint: push.type_hint.as_deref(),
                notification_type: Some(&push.notification_type),
                archive_total_count: None,
            };
            // Re-check right before sending: a newer push may have reached the phone.
            if !with_queue(|q| q.still_pending(&push)) {
                continue;
            }
            match sentryusb_notify::send_mobile_push_only(&config, &req).await {
                Some(Ok(())) => {
                    with_queue(|q| q.record_delivered(&push.notification_type, push.started_at));
                    info!("[notify] mobile push for {} delivered on retry", push.notification_type);
                    record_retry(&push);
                }
                Some(Err(e)) if sentryusb_notify::was_never_sent(&e) => {
                    warn!("[notify] mobile push retry for {} failed: {}", push.notification_type, e)
                }
                Some(Err(e)) => {
                    // It may have reached the server: never risk sending it twice.
                    with_queue(|q| q.sent(&push));
                    warn!("[notify] mobile push retry for {} gave up: {}", push.notification_type, e);
                }
                None => with_queue(|q| q.sent(&push)),
            }
        }
    }
}

fn record_retry(push: &PendingPush) {
    let event = crate::notification_center::NotificationEvent {
        id: String::new(),
        timestamp: 0,
        event_type: push.notification_type.clone(),
        title: push.title.clone(),
        message: push.message.clone(),
        summary: push.summary.clone(),
        provider_errors: HashMap::new(),
        providers: vec!["mobile_push".into()],
        results: HashMap::from([("mobile_push".into(), "ok".into())]),
    };
    if let Err(e) = crate::notification_center::record_event(event) {
        warn!("[notifications] Failed to record retried push in history: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(kind: &str, at: Instant, message: &str) -> PendingPush {
        PendingPush {
            title: "SentryUSB".into(),
            message: message.into(),
            summary: None,
            notification_type: kind.into(),
            type_hint: None,
            started_at: at,
        }
    }

    const EARLY: Duration = Duration::from_secs(30);

    fn drain(q: &mut RetryQueue, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(p) = q.next_due(now, EARLY) {
            out.push(p.message.clone());
            q.sent(&p);
        }
        out
    }

    #[test]
    fn only_listed_types_failing_soon_after_boot_are_kept() {
        let now = Instant::now();
        let mut q = RetryQueue::default();
        assert!(q.record_failure(push("keep_accessory", now, "a"), EARLY));
        assert!(!q.record_failure(push("update", now, "b"), EARLY));
        assert!(!q.record_failure(push("drives", now, "c"), EARLY));
        assert!(!q.record_failure(push("temperature", now, "d"), BOOT_WINDOW));
        assert_eq!(drain(&mut q, now), ["a"]);
    }

    #[test]
    fn only_the_newest_failure_of_each_type_is_sent_one_at_a_time_oldest_first() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_failure(push("archive_error", t0, "old"), EARLY);
        q.record_failure(push("archive_error", t0 + Duration::from_secs(5), "new"), EARLY);
        q.record_failure(push("temperature", t0 + Duration::from_secs(1), "hot"), EARLY);
        let first = q.next_due(t0 + Duration::from_secs(10), EARLY).unwrap();
        assert_eq!(first.message, "hot");
        assert_eq!(q.next_due(t0 + Duration::from_secs(10), EARLY).unwrap().message, "hot", "one push per pass until it is handled");
        q.sent(&first);
        assert_eq!(drain(&mut q, t0 + Duration::from_secs(10)), ["new"]);
    }

    #[test]
    fn an_older_failure_finishing_late_does_not_replace_a_newer_one() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        assert!(q.record_failure(push("keep_accessory", t0 + Duration::from_secs(5), "newer"), EARLY));
        assert!(!q.record_failure(push("keep_accessory", t0, "older"), EARLY));
        assert_eq!(drain(&mut q, t0 + Duration::from_secs(6)), ["newer"]);
    }

    #[test]
    fn a_failure_older_than_a_delivered_push_is_not_queued() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_delivered("keep_accessory", t0 + Duration::from_secs(5));
        assert!(!q.record_failure(push("keep_accessory", t0, "stale"), EARLY));
        assert!(q.record_failure(push("keep_accessory", t0 + Duration::from_secs(9), "fresh"), EARLY));
    }

    #[test]
    fn a_newer_delivered_push_of_the_same_type_drops_the_failed_one() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_failure(push("keep_accessory", t0, "a"), EARLY);
        q.record_delivered("keep_accessory", t0 - Duration::from_secs(1));
        assert!(q.next_due(t0, EARLY).is_some(), "an older delivery does not cancel a newer failure");
        let in_flight = q.next_due(t0, EARLY).unwrap();
        q.record_delivered("keep_accessory", t0 + Duration::from_secs(1));
        assert!(!q.still_pending(&in_flight), "the worker re-checks before sending");
        assert!(q.next_due(t0, EARLY).is_none());
    }

    #[test]
    fn old_pushes_are_dropped_instead_of_sent() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_failure(push("keep_accessory", t0, "a"), EARLY);
        assert!(q.next_due(t0 + MAX_AGE + Duration::from_secs(1), EARLY).is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn retrying_stops_when_the_boot_window_ends() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_failure(push("keep_accessory", t0, "a"), EARLY);
        assert!(q.next_due(t0, BOOT_WINDOW).is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn a_push_replaced_while_sending_stays_queued() {
        let t0 = Instant::now();
        let mut q = RetryQueue::default();
        q.record_failure(push("temperature", t0, "first"), EARLY);
        let sending = q.next_due(t0, EARLY).unwrap();
        q.record_failure(push("temperature", t0 + Duration::from_secs(2), "second"), EARLY);
        q.sent(&sending);
        assert_eq!(drain(&mut q, t0 + Duration::from_secs(3)), ["second"]);
    }
}

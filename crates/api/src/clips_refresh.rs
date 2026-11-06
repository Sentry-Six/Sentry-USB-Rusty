//! On-demand snapshot so Clips can show recent footage away from home.
//!
//! Clips read `/mutable/TeslaCam`, which links into the newest reflink
//! snapshot; on its own that snapshot is up to `SNAPSHOT_INTERVAL` old. A
//! refresh runs exactly what archiveloop's snapshotloop runs: wait for an
//! idle interval in the car's writes, then `make_snapshot.sh`. The USB
//! gadget stays connected, and the snapshots-dir lock serializes the
//! snapshot with archiveloop, releases and link sweeps.
//!
//! * `GET  /api/clips/freshness` — snapshot time and refresh state
//! * `POST /api/clips/refresh`   — start a snapshot when allowed; never waits
//!
//! Both answer 200 with the same JSON, over HTTP and the BLE proxy alike.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;
use tracing::{info, warn};

const SNAPSHOTS_DIR: &str = "/backingfiles/snapshots";

/// Minimum time between snapshots started here, counted from the newest
/// snapshot of any producer and from the last on-demand attempt.
pub(crate) const REFRESH_MIN_INTERVAL_SECS: i64 = 180;

/// Covers the 90 s idle wait, the copy (bounded at 600 s by make_snapshot)
/// and the fsck and link rebuild.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// snapshotloop's sequence, with archiveloop's `log` so the steps land in
/// the same log as scheduled snapshots. An archive cycle that began during
/// the idle wait keeps its own pre-archive snapshot: the check right before
/// `make_snapshot.sh` leaves the snapshot lock to it (`/tmp/archive-cycle`
/// holds `pid:nonce` while a cycle runs, as `ArchiveControl` reads it) and
/// exits 3, so the attempt does not count as a check of the cam disk.
const REFRESH_SCRIPT: &str = r#"source /root/bin/envsetup.sh 2>/dev/null || true
declare -F log > /dev/null 2>&1 || {
  function log { echo "$(date): $*" >> "${LOG_FILE:-/mutable/archiveloop.log}" 2>/dev/null || true; }
  export -f log
}
log "Clips refresh: on-demand snapshot requested"
/root/bin/waitforidle || true
cycle=$(cat /tmp/archive-cycle 2>/dev/null || true)
if [ -n "${cycle%%:*}" ] && [ -d "/proc/${cycle%%:*}" ]; then
  log "Clips refresh: an archive cycle started; leaving the snapshot to it"
  exit 3
fi
/root/bin/make_snapshot.sh
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Freshness {
    /// Unix seconds of the newest committed snapshot, the one Clips reads.
    snapshot_ts: Option<i64>,
    refreshing: bool,
    /// When another on-demand snapshot is allowed; null when allowed now.
    next_refresh_ts: Option<i64>,
    /// When the last on-demand refresh finished comparing the cam disk,
    /// whether or not it found new footage. Null until one has.
    checked_ts: Option<i64>,
    /// The Pi's clock at reply, so ages need not trust the phone's clock.
    server_ts: i64,
}

#[derive(Debug, Default)]
struct RefreshState {
    refreshing: bool,
    last_started: Option<i64>,
    checked: Option<i64>,
}

static STATE: Mutex<RefreshState> =
    Mutex::new(RefreshState { refreshing: false, last_started: None, checked: None });

/// Why a refresh did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blocked {
    Refreshing,
    RateLimited,
    ArchiveRunning,
    SnapshotBusy,
    LowSpace,
}

/// What the Pi is doing that a refresh must not race.
#[derive(Debug, Clone, Copy, Default)]
struct Conditions {
    archive_running: bool,
    snapshot_busy: bool,
    space_ok: bool,
}

/// Earliest time another on-demand snapshot may start. A snapshot time
/// ahead of the clock (after a clock step back) is ignored so it cannot
/// hold refreshes off until the clock catches up.
fn next_allowed(snapshot_ts: Option<i64>, last_started: Option<i64>, now: i64) -> Option<i64> {
    let snapshot_ts = snapshot_ts.filter(|&ts| ts <= now);
    snapshot_ts.max(last_started).map(|base| base + REFRESH_MIN_INTERVAL_SECS)
}

fn freshness(snapshot_ts: Option<i64>, state: &RefreshState, now: i64) -> Freshness {
    Freshness {
        snapshot_ts,
        refreshing: state.refreshing,
        next_refresh_ts: next_allowed(snapshot_ts, state.last_started, now).filter(|&ts| ts > now),
        checked_ts: state.checked,
        server_ts: now,
    }
}

fn may_start(snapshot_ts: Option<i64>, state: &RefreshState, conditions: Conditions, now: i64) -> Result<(), Blocked> {
    if state.refreshing {
        return Err(Blocked::Refreshing);
    }
    if next_allowed(snapshot_ts, state.last_started, now).is_some_and(|ts| ts > now) {
        return Err(Blocked::RateLimited);
    }
    if conditions.archive_running {
        return Err(Blocked::ArchiveRunning);
    }
    if conditions.snapshot_busy {
        return Err(Blocked::SnapshotBusy);
    }
    if !conditions.space_ok {
        return Err(Blocked::LowSpace);
    }
    Ok(())
}

/// `snap.bin` mtime of the newest committed snapshot (one with a TOC; a
/// snapshot still being taken has none yet). `snap.bin`, not the directory:
/// autofs touches the directory when an old snapshot is viewed.
fn latest_snapshot_ts(dir: &Path) -> Option<i64> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("snap-"))
        .filter(|entry| entry.path().join("snap.bin.toc").exists())
        .filter_map(|entry| std::fs::metadata(entry.path().join("snap.bin")).ok()?.modified().ok())
        .filter_map(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_secs() as i64)
        .max()
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn current_conditions() -> Conditions {
    let archive_running = crate::drives_handler::is_archiving()
        || sentryusb_drives::archive_control::ArchiveControl::default().active_cycle().is_some();
    Conditions {
        archive_running,
        snapshot_busy: sentryusb_gadget::snapshot::snapshots_dir_busy(),
        space_ok: sentryusb_gadget::space::meets_default_reserve().unwrap_or_else(|e| {
            warn!("Clips refresh: could not read backing-files space: {e:#}");
            false
        }),
    }
}

/// Clears `refreshing` however the background snapshot ends.
struct RefreshingGuard;

impl Drop for RefreshingGuard {
    fn drop(&mut self) {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).refreshing = false;
    }
}

async fn run_refresh() {
    let _guard = RefreshingGuard;
    match sentryusb_shell::run_with_timeout(REFRESH_TIMEOUT, "bash", &["-c", REFRESH_SCRIPT]).await {
        Ok(out) => {
            // A new snapshot or an identical, discarded one: either way the
            // cam disk was compared. Recorded before `refreshing` clears.
            STATE.lock().unwrap_or_else(|e| e.into_inner()).checked = Some(now_ts());
            if out.trim().is_empty() {
                info!("Clips refresh: no new footage since the last snapshot");
            } else {
                info!("Clips refresh: took {}", out.trim());
            }
        }
        Err(e) => warn!("Clips refresh: snapshot not taken: {e:#}"),
    }
}

async fn read_freshness() -> Freshness {
    let snapshot_ts = tokio::task::spawn_blocking(|| latest_snapshot_ts(Path::new(SNAPSHOTS_DIR)))
        .await
        .unwrap_or(None);
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    freshness(snapshot_ts, &state, now_ts())
}

/// GET /api/clips/freshness
pub async fn get_freshness() -> (StatusCode, Json<Freshness>) {
    (StatusCode::OK, Json(read_freshness().await))
}

/// POST /api/clips/refresh starts an on-demand snapshot when the rate limit,
/// archive state, snapshot lock and space reserve allow, then answers with
/// the current freshness without waiting for it.
pub async fn post_refresh() -> (StatusCode, Json<Freshness>) {
    let snapshot_ts = tokio::task::spawn_blocking(|| latest_snapshot_ts(Path::new(SNAPSHOTS_DIR)))
        .await
        .unwrap_or(None);
    let conditions = tokio::task::spawn_blocking(current_conditions).await.unwrap_or_default();
    let now = now_ts();
    let started = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        match may_start(snapshot_ts, &state, conditions, now) {
            Ok(()) => {
                state.refreshing = true;
                state.last_started = Some(now);
                true
            }
            Err(reason) => {
                info!("Clips refresh: not starting ({reason:?})");
                false
            }
        }
    };
    if started {
        tokio::spawn(run_refresh());
    }
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    (StatusCode::OK, Json(freshness(snapshot_ts, &state, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const CLEAR: Conditions = Conditions { archive_running: false, snapshot_busy: false, space_ok: true };

    fn idle(last_started: Option<i64>) -> RefreshState {
        RefreshState { refreshing: false, last_started, checked: None }
    }

    #[test]
    fn response_has_exactly_the_app_contract_fields() {
        let value = serde_json::to_value(freshness(Some(NOW - 3_000), &idle(None), NOW)).unwrap();
        assert_eq!(value, serde_json::json!({
            "snapshot_ts": NOW - 3_000,
            "refreshing": false,
            "next_refresh_ts": null,
            "checked_ts": null,
            "server_ts": NOW,
        }));
        let none = serde_json::to_value(freshness(None, &idle(None), NOW)).unwrap();
        assert_eq!(none, serde_json::json!({
            "snapshot_ts": null, "refreshing": false, "next_refresh_ts": null, "checked_ts": null, "server_ts": NOW,
        }));
    }

    #[test]
    fn a_refresh_that_found_nothing_new_reports_when_it_checked() {
        // Parked car: the refresh discarded an identical snapshot, so the
        // snapshot stays old but the check is recent.
        let state = RefreshState { refreshing: false, last_started: Some(NOW - 100), checked: Some(NOW - 5) };
        let value = serde_json::to_value(freshness(Some(NOW - 3_000), &state, NOW)).unwrap();
        assert_eq!(value["snapshot_ts"], NOW - 3_000);
        assert_eq!(value["checked_ts"], NOW - 5);
        assert_eq!(value["server_ts"], NOW);
        assert_eq!(value["next_refresh_ts"], NOW + 80);
    }

    #[test]
    fn rate_limit_counts_from_the_newest_snapshot_and_the_last_attempt() {
        // A snapshot two minutes old (any producer) holds refreshes off.
        let recent = Some(NOW - 120);
        assert_eq!(may_start(recent, &idle(None), CLEAR, NOW), Err(Blocked::RateLimited));
        assert_eq!(freshness(recent, &idle(None), NOW).next_refresh_ts, Some(NOW + 60));
        // Exactly the interval later is allowed.
        assert_eq!(may_start(Some(NOW - REFRESH_MIN_INTERVAL_SECS), &idle(None), CLEAR, NOW), Ok(()));
        // An attempt that found no new footage leaves the snapshot old but
        // still rate-limits the next attempt.
        let old = Some(NOW - 3_000);
        assert_eq!(may_start(old, &idle(Some(NOW - 30)), CLEAR, NOW), Err(Blocked::RateLimited));
        assert_eq!(freshness(old, &idle(Some(NOW - 30)), NOW).next_refresh_ts, Some(NOW + 150));
        assert_eq!(may_start(old, &idle(Some(NOW - 200)), CLEAR, NOW), Ok(()));
        // No snapshot yet and no attempt: allowed now.
        assert_eq!(may_start(None, &idle(None), CLEAR, NOW), Ok(()));
    }

    #[test]
    fn a_snapshot_dated_in_the_future_does_not_block_refreshes() {
        let ahead = Some(NOW + 7_200);
        assert_eq!(may_start(ahead, &idle(None), CLEAR, NOW), Ok(()));
        assert_eq!(freshness(ahead, &idle(None), NOW).next_refresh_ts, None);
    }

    #[test]
    fn refresh_waits_for_archive_lock_and_space_and_never_overlaps() {
        let old = Some(NOW - 3_000);
        let busy = RefreshState { refreshing: true, last_started: Some(NOW - 400), checked: None };
        assert_eq!(may_start(old, &busy, CLEAR, NOW), Err(Blocked::Refreshing));
        assert!(freshness(old, &busy, NOW).refreshing);
        for (conditions, blocked) in [
            (Conditions { archive_running: true, ..CLEAR }, Blocked::ArchiveRunning),
            (Conditions { snapshot_busy: true, ..CLEAR }, Blocked::SnapshotBusy),
            (Conditions { space_ok: false, ..CLEAR }, Blocked::LowSpace),
        ] {
            assert_eq!(may_start(old, &idle(None), conditions, NOW), Err(blocked));
        }
    }

    #[test]
    fn refresh_script_parses_and_keeps_snapshotloops_order() {
        let status = std::process::Command::new("bash").args(["-n", "-c", REFRESH_SCRIPT]).status().unwrap();
        assert!(status.success());
        let idle = REFRESH_SCRIPT.find("/root/bin/waitforidle").unwrap();
        let archive_check = REFRESH_SCRIPT.find("/tmp/archive-cycle").unwrap();
        let snapshot = REFRESH_SCRIPT.find("/root/bin/make_snapshot.sh").unwrap();
        assert!(idle < archive_check && archive_check < snapshot);
        // Stepping aside for an archive cycle is not a check of the cam disk.
        assert!(REFRESH_SCRIPT[archive_check..snapshot].contains("exit 3"));
    }

    #[test]
    fn latest_snapshot_ignores_uncommitted_and_unrelated_entries() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("sentryusb-clips-fresh-{}-{nonce}", std::process::id()));
        let snap = |name: &str, toc: bool| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("snap.bin"), b"").unwrap();
            if toc {
                std::fs::write(dir.join("snap.bin.toc"), b"").unwrap();
            }
            dir
        };
        assert_eq!(latest_snapshot_ts(&root), None);
        let committed = snap("snap-000001", true);
        snap("snap-000002", false); // still being taken
        std::fs::create_dir_all(root.join("lost+found")).unwrap();
        let expected = std::fs::metadata(committed.join("snap.bin")).unwrap().modified().unwrap()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert_eq!(latest_snapshot_ts(&root), Some(expected));
        std::fs::remove_dir_all(root).unwrap();
    }
}

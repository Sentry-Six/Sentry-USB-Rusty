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

/// Shell helpers for the refresh, kept separate so the tests can run them
/// against a fake `/proc` and snapshots dir.
///
/// `car_quiet` succeeds when the car has provably stopped writing:
/// waitforidle only ends early after it has seen a burst followed by quiet,
/// so a parked car that is not recording always cost its full 90 s (and its
/// timeout then snapshots without any idle evidence). This check needs seven
/// consecutive one-second samples of one `file-storage` process whose
/// `write_bytes` grew by at most 64 KiB each; a recording car writes tens of
/// MB per second. Any unreadable or non-numeric sample, counter decrease or
/// PID change restarts the run, and no complete run within 14 samples falls
/// back to waitforidle. No `file-storage` process at all means the gadget
/// is not exposing the disk, so nothing can be writing to it.
///
/// `committed_snaps` lists slots with a committed TOC; `new_snapshot`
/// prints the one slot this run committed, or fails when this run left an
/// uncommitted slot behind: a new slot, or one whose image (inode, mtime,
/// size) changed, as when make_snapshot reuses a slot. Untouched leftovers,
/// including a mounted incomplete slot that make_snapshot keeps and appends
/// after, are not this run's. The helpers always return 0 so `set -e`
/// callers stay safe.
macro_rules! refresh_helpers {
    () => {
        r#"car_quiet() {
  local root=${CLIPS_PROC_ROOT:-/proc} nap=${CLIPS_SLEEP:-1}
  local quiet=0 prev=-1 pid="" out rc w
  for _ in $(seq 1 14); do
    sleep "$nap"
    out=$(pgrep -x file-storage 2>/dev/null); rc=$?
    if [ "$rc" -eq 1 ] && [ -z "$out" ]; then return 0; fi
    w=""
    if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9]+$ ]]; then
      w=$(awk '$1 == "write_bytes:" { print $2; exit }' "$root/$out/io" 2>/dev/null)
    fi
    if ! [[ "$w" =~ ^[0-9]+$ ]] || { [ "$prev" -ge 0 ] && { [ "$out" != "$pid" ] || [ "$w" -lt "$prev" ]; }; }; then
      quiet=0; prev=-1; pid=""; continue
    fi
    if [ "$prev" -lt 0 ]; then prev=$w; pid=$out; continue; fi
    if [ $((w - prev)) -le 65536 ]; then quiet=$((quiet + 1)); else quiet=0; fi
    prev=$w
    [ "$quiet" -ge 7 ] && return 0
  done
  return 1
}
all_snaps() {
  local d
  for d in "${CLIPS_SNAP_DIR:-/backingfiles/snapshots}"/snap-*; do
    if [[ "${d##*/}" =~ ^snap-[0-9]+$ ]] && [ -d "$d" ]; then echo "${d##*/}"; fi
  done | LC_ALL=C sort
  return 0
}
committed_snaps() {
  local n
  for n in $(all_snaps); do
    if [ -e "${CLIPS_SNAP_DIR:-/backingfiles/snapshots}/$n/snap.bin.toc" ]; then echo "$n"; fi
  done
  return 0
}
uncommitted_marks() {
  local n f dir=${CLIPS_SNAP_DIR:-/backingfiles/snapshots}
  for n in $(LC_ALL=C comm -23 <(all_snaps) <(committed_snaps)); do
    f="$dir/$n/snap.bin"
    if [ ! -e "$f" ]; then f="$dir/$n"; fi
    echo "$n $(stat -c '%i %y %s' "$f" 2>/dev/null)"
  done | LC_ALL=C sort
  return 0
}
new_snapshot() {
  local before_committed=$1 before_marks=$2 new failed
  new=$(LC_ALL=C comm -13 <(printf '%s\n' "$before_committed" | LC_ALL=C sort) <(committed_snaps))
  if [ -n "$new" ] && [ "$(wc -l <<< "$new")" -eq 1 ]; then echo "$new"; return 0; fi
  failed=$(LC_ALL=C comm -13 <(printf '%s\n' "$before_marks" | LC_ALL=C sort) <(uncommitted_marks) | head -1)
  if [ -n "$failed" ]; then
    echo "snapshot ${failed%% *} was not completed" >&2
    return 4
  fi
  return 0
}
"#
    };
}

/// snapshotloop's sequence, with archiveloop's `log` so the steps land in
/// the same log as scheduled snapshots, and the quick quiet check from
/// `refresh_helpers` in front of waitforidle. An archive cycle that began
/// during the idle wait keeps its own pre-archive snapshot: the check right
/// before `make_snapshot.sh` leaves the snapshot lock to it
/// (`/tmp/archive-cycle` holds `pid:nonce` while a cycle runs, as
/// `ArchiveControl` reads it) and exits 3, so the attempt does not count as
/// a check of the cam disk. The refresh holds the snapshots-dir lock itself
/// around `make_snapshot.sh` (which then skips its own lock), so the slot it
/// reports is the one this run committed.
const REFRESH_SCRIPT: &str = concat!(
    r#"source /root/bin/envsetup.sh 2>/dev/null || true
declare -F log > /dev/null 2>&1 || {
  function log { echo "$(date): $*" >> "${LOG_FILE:-/mutable/archiveloop.log}" 2>/dev/null || true; }
  export -f log
}
"#,
    refresh_helpers!(),
    r#"log "Clips refresh: on-demand snapshot requested"
if car_quiet; then
  log "Clips refresh: car is not writing, taking the snapshot now"
else
  /root/bin/waitforidle || true
fi
cycle=$(cat /tmp/archive-cycle 2>/dev/null || true)
if [ -n "${cycle%%:*}" ] && [ -d "/proc/${cycle%%:*}" ]; then
  log "Clips refresh: an archive cycle started; leaving the snapshot to it"
  exit 3
fi
mkdir -p /backingfiles/snapshots
exec 9< /backingfiles/snapshots
flock -E 99 9 || { echo "failed to lock snapshots dir" >&2; exit 99; }
before_committed=$(committed_snaps)
before_marks=$(uncommitted_marks)
FLOCKED=/root/bin/make_snapshot.sh /root/bin/make_snapshot.sh > /dev/null || exit $?
new_snapshot "$before_committed" "$before_marks"
"#
);

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

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("sentryusb-clips-{tag}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Runs `car_quiet` with a fake `pgrep` that walks `samples`: "none" (no
    /// process), "err" (pgrep failed), or "pid:value" where value is the
    /// `write_bytes` count, "unreadable" or any text.
    fn car_quiet(samples: &[&str]) -> bool {
        let root = temp_root("quiet");
        std::fs::write(root.join("samples"), samples.join("\n") + "\n").unwrap();
        let script = format!(
            r#"{helpers}
ROOT='{root}'
pgrep() {{
  local i e pid v
  i=$(cat "$ROOT/i" 2>/dev/null || echo 0); echo $((i + 1)) > "$ROOT/i"
  e=$(sed -n "$((i + 1))p" "$ROOT/samples")
  case "$e" in none|"") return 1;; err) return 2;; esac
  pid=${{e%%:*}}; v=${{e#*:}}
  mkdir -p "$ROOT/proc/$pid"
  if [ "$v" = unreadable ]; then rm -f "$ROOT/proc/$pid/io"; else printf 'rchar: 0\nwrite_bytes: %s\n' "$v" > "$ROOT/proc/$pid/io"; fi
  echo "$pid"
}}
CLIPS_PROC_ROOT="$ROOT/proc" CLIPS_SLEEP=0 car_quiet"#,
            helpers = refresh_helpers!(),
            root = root.display()
        );
        let ok = std::process::Command::new("bash").args(["-c", &script]).status().unwrap().success();
        std::fs::remove_dir_all(&root).ok();
        ok
    }

    fn series(pid: u32, start: u64, step: u64, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{pid}:{}", start + step * i as u64)).collect()
    }

    #[test]
    fn quiet_check_skips_only_with_clear_evidence() {
        let run = |v: Vec<String>| car_quiet(&v.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(car_quiet(&["none"]), "no mass-storage process");
        assert!(run(series(7, 1000, 0, 8)), "no writes");
        assert!(run(series(7, 1000, 4096, 8)), "4 KiB/s trickle");
        assert!(!run(series(7, 1000, 3_000_000, 14)), "recording");
        assert!(!run(series(7, 1000, 100_000, 14)), "100 KB/s trickle");
        assert!(!car_quiet(&["err"; 14]), "pgrep failing is not 'no process'");
        assert!(!car_quiet(&["7\n8:0"; 14]), "two processes");
    }

    #[test]
    fn quiet_check_restarts_on_bad_samples() {
        for (bad, pid, next) in [("7:unreadable", 7, 1000), ("7:abc", 7, 1000), ("7:10", 7, 10), ("9:1000", 9, 1000)] {
            let mut v: Vec<String> = series(7, 1000, 0, 6);
            v.push(bad.to_string());
            v.extend(series(pid, next, 0, 7));
            assert_eq!(v.len(), 14);
            assert!(!car_quiet(&v.iter().map(String::as_str).collect::<Vec<_>>()), "{bad} must restart the run");
        }
        let mut late: Vec<String> = vec!["7:unreadable".into()];
        late.extend(series(7, 1000, 0, 8));
        assert!(car_quiet(&late.iter().map(String::as_str).collect::<Vec<_>>()), "a full run after a bad sample");
    }

    /// Lays out `before` slots ("name", "name!" for uncommitted), records
    /// them as the refresh does, then applies `after` (a trailing '*'
    /// recreates the slot, as make_snapshot does when it reuses one) and
    /// returns new_snapshot's (exit code, stdout).
    fn new_snapshot(before: &[&str], after: &[&str]) -> (i32, String) {
        let root = temp_root("snaps");
        let lay = |list: &[&str]| {
            let parsed: Vec<(String, bool, bool)> = list
                .iter()
                .map(|s| {
                    let recreate = s.ends_with('*');
                    let s = s.trim_end_matches('*');
                    let committed = !s.ends_with('!');
                    (s.trim_end_matches('!').to_string(), committed, recreate)
                })
                .collect();
            for e in std::fs::read_dir(&root).unwrap() {
                let path = e.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                if !parsed.iter().any(|(n, _, r)| *n == name && !r) {
                    std::fs::remove_dir_all(&path).unwrap();
                }
            }
            for (name, committed, _) in parsed {
                let dir = root.join(&name);
                std::fs::create_dir_all(&dir).unwrap();
                if !dir.join("snap.bin").exists() {
                    std::fs::write(dir.join("snap.bin"), b"").unwrap();
                }
                let toc = dir.join("snap.bin.toc");
                if committed {
                    std::fs::write(&toc, b"").unwrap();
                } else {
                    std::fs::remove_file(&toc).ok();
                }
            }
        };
        let run = |body: &str| {
            let script = format!("{}\nexport CLIPS_SNAP_DIR='{}'\n{body}", refresh_helpers!(), root.display());
            std::process::Command::new("bash").args(["-c", &script]).output().unwrap()
        };
        lay(before);
        let committed = String::from_utf8(run("set -e; c=$(committed_snaps); echo \"$c\"").stdout).unwrap();
        let marks = String::from_utf8(run("set -e; m=$(uncommitted_marks); echo \"$m\"").stdout).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        lay(after);
        let out = run(&format!("set -e\nnew_snapshot '{}' '{}'", committed.trim_end(), marks.trim_end()));
        std::fs::remove_dir_all(&root).ok();
        (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap().trim().to_string())
    }

    #[test]
    fn snapshot_name_is_only_the_slot_this_run_committed() {
        let base = ["snap-000001", "snap-000002"];
        assert_eq!(new_snapshot(&base, &base), (0, String::new()), "identical snapshot discarded");
        assert_eq!(new_snapshot(&base, &["snap-000001", "snap-000002", "snap-000003"]), (0, "snap-000003".into()));
        assert_eq!(new_snapshot(&base, &["snap-000002", "snap-000003"]), (0, "snap-000003".into()), "old slot released for space");
        assert_eq!(new_snapshot(&base, &["snap-000001", "snap-000002", "snap-000003!"]).0, 4, "new slot left uncommitted");
        assert_eq!(new_snapshot(&["snap-000001", "snap-000002!"], &["snap-000001", "snap-000002!*"]).0, 4, "reused slot failed again");
        assert_eq!(new_snapshot(&["snap-000001", "snap-000002!"], &["snap-000001", "snap-000002*"]), (0, "snap-000002".into()), "reused slot committed");
        assert_eq!(
            new_snapshot(&["snap-000001", "snap-000002!"], &["snap-000001", "snap-000002!", "snap-000003"]),
            (0, "snap-000003".into()),
            "mounted incomplete max slot kept, new slot appended and committed"
        );
        assert_eq!(
            new_snapshot(&["snap-000001", "snap-000002!"], &["snap-000001", "snap-000002!"]),
            (0, String::new()),
            "mounted incomplete max slot kept, appended snapshot identical"
        );
        assert_eq!(
            new_snapshot(&["snap-000001!", "snap-000002"], &["snap-000001!", "snap-000002", "snap-000003"]),
            (0, "snap-000003".into()),
            "an old stale slot is not this run's failure"
        );
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

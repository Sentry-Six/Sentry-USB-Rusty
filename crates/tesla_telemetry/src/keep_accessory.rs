//! Keep-Accessory-Power automation policy.
//!
//! "Keep Accessory Power" (the toggle on the Tesla app's Charging
//! screen) holds the car's 12V accessory outlets (cigarette lighter)
//! live after the car would otherwise cut them. For a Pi powered from
//! that outlet — NOT the glovebox USB, which the car feeds itself
//! during Sentry — this is what keeps the Pi alive while parked.
//!
//! This policy auto-manages that toggle from signals the sampler
//! already computes each tick, so the user never touches it:
//!
//!   * **Away from home → ON.** Pre-armed the moment you leave the
//!     home geofence (still moving), so it's already on before you
//!     park and the car cuts accessory power. Stays on the whole time
//!     parked away → Sentry/dashcam coverage with the Pi powered.
//!   * **Home + parked → hold ON until the archive finishes, then
//!     OFF.** Guarantees the archive completes with power, then lets
//!     the car sleep and the Pi power down cleanly (no half-archives,
//!     no battery drain sitting in your own garage).
//!
//! Gated behind `KEEP_ACCESSORY_ENABLED` (a user-declared "my Pi is
//! powered from the 12V outlet" flag) + a home geofence
//! (`KEEP_ACCESSORY_HOME_LAT/LON`, `..._RADIUS_M`). A radius circle —
//! not an address-string match — because Tesla's reverse-geocoded
//! address drifts (it'll say you're "at your neighbor's"); the
//! geofence swallows that jitter.
//!
//! The Tesla protocol is **write-only** for this field — there's no
//! readable "is keep-accessory on?" state — so the policy tracks its
//! OWN last-sent value and only issues a BLE action on a change.

use std::time::{Duration, Instant};

use sentryusb_tesla_ble::{actions, manager::PersistentSession};
use tracing::{info, warn};

use crate::config::KeepAccessoryConfig;
use crate::sample::ChargingState;

/// After arriving home + parked, how long to keep power on waiting for
/// an archive to START before concluding there's nothing to archive
/// and turning off. Covers archiveloop's settle/spawn latency.
const ARCHIVE_START_GRACE: Duration = Duration::from_secs(5 * 60);

/// Absolute cap on a single charge-hold (bounds drain if the car sits plugged-in-but-idle).
const CHARGE_HOLD_MAX: Duration = Duration::from_secs(18 * 60 * 60);

/// Cadence for the persistent charge-hold heartbeat (durable last-seen across a brownout).
const CHARGE_HOLD_HEARTBEAT: Duration = Duration::from_secs(5 * 60);

/// How many consecutive BLE send failures before we push a "couldn't set
/// accessory power" alert — high enough that a transient blip that
/// self-recovers on the next retry stays silent.
const FAIL_NOTIFY_STREAK: u32 = 4;

/// Policy state that must persist across ticks. Lives in `main()`.
#[derive(Default)]
pub struct KeepAccessoryState {
    /// What we last commanded. Write-only protocol → we only send when
    /// the desired value differs from this.
    last_sent: Option<bool>,
    /// When we first became home+parked this session (`None` while
    /// away or still moving). Anchors the archive-start grace window.
    home_arrival: Option<Instant>,
    /// True once an archive has been observed running since arriving
    /// home — so we hold power until it finishes, then release.
    archive_seen_active: bool,
    /// One-shot guard so we fire the "Pi going offline" push exactly once
    /// per home-release cycle (re-armed whenever we go back ON/away).
    offline_notified: bool,
    /// One-shot guard for the "Sentry coverage active" push — fired once
    /// when we power ON because we left home (re-armed on return home).
    coverage_notified: bool,
    /// Consecutive BLE send failures (reset to 0 on any success). Used to
    /// suppress the failure push for transient blips that self-recover.
    send_fail_streak: u32,
    /// One-shot guard so the "couldn't set accessory power" push fires once
    /// per failure streak, not every retry tick (reset on success).
    fail_notified: bool,
    /// When we began holding 12V for a charge (`None` while not eligible).
    charge_hold_since: Option<Instant>,
    /// When the charge first read `Complete` this hold (`None` until then).
    charge_complete_at: Option<Instant>,
    /// Last charge-hold heartbeat write (`None` until the first).
    charge_hb_at: Option<Instant>,
    /// One-shot guard for the "holding power for charge" push (re-armed when
    /// no longer holding for charge).
    charge_hold_notified: bool,
    /// One-shot guard for the "charge complete — releasing soon" push
    /// (re-armed when no longer holding for charge).
    charge_grace_notified: bool,
}

/// Great-circle distance in meters (haversine).
fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_R_M: f64 = 6_371_000.0;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dphi = (lat2 - lat1).to_radians();
    let dlambda = (lon2 - lon1).to_radians();
    let a =
        (dphi / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlambda / 2.0).sin().powi(2);
    2.0 * EARTH_R_M * a.sqrt().asin()
}

/// Pure decision core. `Some((desired, reason))` to set keep-accessory,
/// or `None` to leave it untouched (home but still moving — the car is
/// awake on its own, accessory power is irrelevant right now).
///
/// Kept pure (no I/O, no time) so the home→OFF logic is unit-tested
/// without a live test that would power the Pi off.
///
/// The `charge_*` flags are false unless `hold_for_charge` is on and the cable is connected.
#[allow(clippy::too_many_arguments)]
fn decide_desired(
    is_home: bool,
    parked: bool,
    archive_active: bool,
    archive_seen_active: bool,
    grace_expired: bool,
    charge_hold: bool,
    charge_complete_grace: bool,
    charge_done: bool,
) -> Option<(bool, &'static str)> {
    if !is_home {
        // Away: pre-arm/hold ON (Sentry coverage while parked away).
        Some((true, "away from home"))
    } else if !parked {
        // Home but still moving (arriving / maneuvering) — leave it.
        None
    } else if archive_active {
        Some((true, "home, archiving — holding power"))
    } else if charge_hold {
        // Plugged in — hold 12V instead of releasing after the archive.
        Some((true, "home, plugged in — holding for charge"))
    } else if charge_complete_grace {
        Some((true, "home, charge complete — releasing soon"))
    } else if charge_done {
        Some((false, "home, charge complete — releasing"))
    } else if archive_seen_active {
        Some((false, "home, archive finished — releasing"))
    } else if !grace_expired {
        Some((true, "home, waiting for archive to start"))
    } else {
        Some((false, "home, nothing to archive — releasing"))
    }
}

/// Evaluate + enforce the policy for this tick. Best-effort: only acts
/// on a state change, only when the radio is held (so the held car
/// session is live), and silently holds when there's no GPS fix.
///
/// * `lat`/`lon` — last known GPS (`None` → no fix yet, hold)
/// * `parked` — car parked/settled. On HW3 `shift_state` reads
///   `Unknown` when parked, so the caller passes
///   `parked_polls >= N || car_truly_asleep` (the daemon's own
///   quiet-mode signal), NOT shift state alone.
/// * `archive_active` — `/tmp/archive_status.json` fresh (archiveloop running)
/// * `radio_held` — the daemon currently owns the radio (session usable)
/// * `charging_state` — last decoded charge state (only used when `hold_for_charge` is on)
#[allow(clippy::too_many_arguments)]
pub async fn evaluate(
    cfg: &KeepAccessoryConfig,
    session: &PersistentSession,
    state: &mut KeepAccessoryState,
    lat: Option<f64>,
    lon: Option<f64>,
    parked: bool,
    archive_active: bool,
    radio_held: bool,
    charging_state: Option<ChargingState>,
) {
    if !cfg.enabled {
        return; // feature off — the "Pi powered from 12V" gate
    }
    let (Some(home_lat), Some(home_lon)) = (cfg.home_lat, cfg.home_lon) else {
        return; // home geofence not set yet → policy inert
    };

    // No GPS fix this eval → hold current state (don't flip on a
    // transient missing reading; parked polls legitimately omit coords).
    let (Some(la), Some(lo)) = (lat, lon) else {
        return;
    };
    let is_home = distance_m(la, lo, home_lat, home_lon) <= cfg.home_radius_m;

    // Home-arrival + archive bookkeeping.
    if is_home && parked {
        if state.home_arrival.is_none() {
            state.home_arrival = Some(Instant::now());
            state.archive_seen_active = false;
        }
    } else {
        state.home_arrival = None;
        state.archive_seen_active = false;
    }
    if archive_active {
        state.archive_seen_active = true;
    }

    let grace_expired = state
        .home_arrival
        .map(|t| t.elapsed() >= ARCHIVE_START_GRACE)
        .unwrap_or(false);

    // Charge-hold bookkeeping: track hold-start (cap) + first Complete (grace) while eligible.
    let cable_connected = cfg.hold_for_charge
        && matches!(
            charging_state,
            Some(
                ChargingState::Starting
                    | ChargingState::Charging
                    | ChargingState::Calibrating
                    | ChargingState::Complete
                    | ChargingState::Stopped
                    | ChargingState::NoPower
            )
        );
    let charge_eligible = cfg.hold_for_charge && is_home && parked && cable_connected;
    // A charging car parked at home does not un-park. The sampler's `parked`
    // can still flicker false mid-charge (HW3 reads shift_state=Unknown, and
    // Tesla's periodic wake window briefly un-quiets USB), so a transient
    // ineligibility while we're still home + plugged in is not authoritative.
    // Hold the charge state steady through it instead of tearing down and
    // re-firing the entry push. Only a real exit (left home / cable unplugged)
    // releases.
    let charge_session_live = cfg.hold_for_charge && is_home && cable_connected;
    if charge_eligible {
        if state.charge_hold_since.is_none() {
            state.charge_hold_since = Some(Instant::now());
            state.charge_complete_at = None;
        }
        if charging_state == Some(ChargingState::Complete) && state.charge_complete_at.is_none() {
            state.charge_complete_at = Some(Instant::now());
        }
    } else if state.charge_hold_since.is_some() && charge_session_live {
        // transient ineligibility — hold steady, do not re-arm notifications
    } else {
        state.charge_hold_since = None;
        state.charge_complete_at = None;
        state.charge_hold_notified = false;
        state.charge_grace_notified = false;
    }
    // Post-complete release grace, runtime-configurable (default 45m).
    let charge_grace = Duration::from_secs(cfg.charge_grace_min.saturating_mul(60));
    let hold_capped = state
        .charge_hold_since
        .map(|t| t.elapsed() >= CHARGE_HOLD_MAX)
        .unwrap_or(false);
    let complete_grace_active = state
        .charge_complete_at
        .map(|t| t.elapsed() < charge_grace)
        .unwrap_or(false);
    // Mutually-exclusive charge signals: holding / in release-grace / done.
    let charge_done = charge_eligible
        && (hold_capped || (state.charge_complete_at.is_some() && !complete_grace_active));
    let charge_complete_grace = charge_eligible && !charge_done && complete_grace_active;
    let charge_hold = charge_eligible && !charge_done && !charge_complete_grace;

    // Charge-hold pushes: announce entering the hold and the post-complete
    // grace once each. The notified flags reset only on a real teardown
    // (cable unplugged / left home), so a quiet-window flicker can't
    // double-fire the "Holding power for charge" push.
    if charge_hold && !state.charge_hold_notified {
        notify_event(
            "Charging power requested",
            "Plugged in at home — requesting accessory power so the Pi can stay available while the car charges.",
            "Requesting device power while charging.",
        )
        .await;
        state.charge_hold_notified = true;
    }
    if charge_complete_grace && !state.charge_grace_notified {
        let msg = format!(
            "Charging complete — releasing accessory power in ~{} minutes, then the Pi powers down.",
            cfg.charge_grace_min
        );
        let summary = format!("Charging complete. Releasing device power in about {} minutes.", cfg.charge_grace_min);
        notify_event("Charge complete", &msg, &summary).await;
        state.charge_grace_notified = true;
    }

    // Throttled heartbeat so a brownout mid-hold leaves a durable last-seen line.
    if charge_hold || charge_complete_grace {
        let due = state
            .charge_hb_at
            .map(|t| t.elapsed() >= CHARGE_HOLD_HEARTBEAT)
            .unwrap_or(true);
        if due {
            state.charge_hb_at = Some(Instant::now());
            let line = if charge_complete_grace {
                let rem_min = state
                    .charge_complete_at
                    .map(|t| charge_grace.saturating_sub(t.elapsed()).as_secs() / 60)
                    .unwrap_or(0);
                format!("charge-hold heartbeat: complete — releasing in ~{rem_min}m")
            } else {
                let held_min = state
                    .charge_hold_since
                    .map(|t| t.elapsed().as_secs() / 60)
                    .unwrap_or(0);
                format!("charge-hold heartbeat: holding for charge ({held_min}m held)")
            };
            crate::diag_log::log_event(&format!("keep-accessory: {line}"));
            log_persistent(&line);
        }
    } else {
        state.charge_hb_at = None;
    }

    let Some((desired, why)) = decide_desired(
        is_home,
        parked,
        archive_active,
        state.archive_seen_active,
        grace_expired,
        charge_hold,
        charge_complete_grace,
        charge_done,
    ) else {
        return; // no change warranted (home but moving)
    };

    // Coverage notification keys on the home→away transition, NOT the
    // power-state change — fire it even when the Pi is already ON. Observed
    // live 2026-05-31: the Pi booted at home into the archive-grace ON state,
    // then drove away while still ON, so the power state went ON→ON with no
    // change and the `last_sent == desired` early-return below skipped the
    // coverage push entirely. It's just a push (no car radio needed), so it
    // runs before both the no-change and radio-held returns.
    if is_home {
        state.coverage_notified = false; // back home → re-arm coverage push
    }
    if desired && !is_home && !state.coverage_notified {
        notify_event(
            "Sentry power requested",
            "Parked away from home — requesting accessory power so the Pi can stay available for Sentry.",
            "Requesting device power for Sentry while parked away from home.",
        )
        .await;
        state.coverage_notified = true;
    }

    if state.last_sent == Some(desired) {
        return; // already in the desired state
    }

    // The send needs a live car session; we only have one while the
    // daemon holds the radio. If we don't, leave last_sent untouched so
    // we retry on a tick that does. (The race-critical AWAY→ON happens
    // during Active driving, where the radio is always held.)
    if !radio_held {
        return;
    }

    // Offline ("Pi going offline") notification — must fire synchronously
    // right before the OFF send, while we still have power + the radio. The
    // coverage ("Sentry coverage active") push is handled earlier (before the
    // no-change return) since it keys on the home→away transition, not the send.
    if desired {
        state.offline_notified = false; // power ON → re-arm offline push
    }
    if !desired && !state.offline_notified {
        // Home release cuts our 12V the instant it lands, so fire BEFORE
        // sending OFF and synchronously (the helper blocks until the local
        // server dispatched the push) so it egresses while we still have
        // power. One-shot per home cycle.
        //
        // Word the message to the actual release reason: an archive doesn't
        // always run at home (no new footage / keep-awake handled it), so
        // `archive_seen_active` distinguishes "archive finished" from the
        // grace-expired "nothing to archive" path. (Mirrors decide_desired's
        // two home-OFF branches.)
        let msg = if state.archive_seen_active {
            "Archive activity ended at home — requesting release of accessory power. The Pi may power down until the car wakes again."
        } else {
            "Back home — requesting release of accessory power. The Pi may power down until the car wakes again."
        };
        notify_event("Releasing device power", msg, "Releasing device power. The device may go offline.").await;
        state.offline_notified = true;
    }

    let label = if desired { "ON" } else { "OFF" };
    info!("keep-accessory: policy → {} ({})", label, why);
    match session
        .send_action(actions::set_keep_accessory_power(desired))
        .await
    {
        Ok(_) => {
            state.last_sent = Some(desired);
            state.send_fail_streak = 0;
            state.fail_notified = false; // recovered → re-arm the failure push
            info!("keep-accessory: set {} ok", label);
            // Persist to the BLE log (survives reboots; shows in
            // Logs → Bluetooth) so parked-power behavior is reviewable
            // after a power cut wipes the volatile journal.
            crate::diag_log::log_event(&format!(
                "keep-accessory: -> {label} ({why}) [sent ok]"
            ));
            log_persistent(&format!("-> {label} ({why}) [sent ok]"));
        }
        Err(e) => {
            warn!(
                "keep-accessory: set {} failed: {:#} (retry next tick)",
                label, e
            );
            crate::diag_log::log_event(&format!(
                "keep-accessory: -> {label} ({why}) [SEND FAILED: {e:#}]"
            ));
            log_persistent(&format!("-> {label} ({why}) [SEND FAILED: {e:#}]"));
            // Anti-spam: only push after several consecutive failures (a
            // transient blip that recovers on the next retry stays silent),
            // and only once per streak.
            state.send_fail_streak = state.send_fail_streak.saturating_add(1);
            if state.send_fail_streak >= FAIL_NOTIFY_STREAK && !state.fail_notified {
                let detail = if desired {
                    "Couldn't turn Keep Accessory Power ON — the car is unreachable over BLE, so Sentry coverage may not be armed. Still retrying."
                } else {
                    "Couldn't turn Keep Accessory Power OFF at home — the car is unreachable over BLE, so the Pi may stay powered. Still retrying."
                };
                let summary = if desired {
                    "Could not keep device power on. Sentry coverage may be affected. Retrying."
                } else {
                    "Could not release device power. The device may stay on. Retrying."
                };
                notify_event("Keep Accessory issue", detail, summary).await;
                state.fail_notified = true;
            }
        }
    }
}

/// Fire a `keep_accessory` push through the local Notification Center
/// (`POST /api/notifications/send`), which fans out to every channel the
/// user configured — including the Sentry Connect mobile app. Gated
/// server-side by the `keep_accessory` toggle.
///
/// Runs `curl` on a blocking thread and awaits it, so for the going-offline
/// case we don't proceed to cut our own 12V power until the push has been
/// handed off (`send_notification` awaits the actual egress). Best-effort:
/// any failure is swallowed.
async fn notify_event(title: &str, message: &str, summary: &str) {
    let body = serde_json::json!({
        "notification_type": "keep_accessory",
        "title": title,
        "message": message,
        "summary": summary,
    })
    .to_string();
    let title_owned = title.to_string();
    let res = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .args([
                "-s",
                "--max-time",
                "15",
                "-X",
                "POST",
                "http://localhost/api/notifications/send",
                "-H",
                "Content-Type: application/json",
                "-d",
                &body,
            ])
            .output()
    })
    .await;
    match res {
        Ok(Ok(o)) if o.status.success() => {
            log_persistent(&format!("notify: \"{title_owned}\" push dispatched"));
        }
        other => {
            log_persistent(&format!("notify: \"{title_owned}\" push FAILED ({other:?})"));
        }
    }
}

/// Append a keep-accessory event to a DEDICATED persistent log
/// (`/mutable/keep-accessory.log`). Separate from the high-volume,
/// rotating BLE diag log (which trims its older half) so these
/// low-frequency events aren't lost — they must survive reboots for
/// parked-power diagnosis.
fn log_persistent(event: &str) {
    use std::io::Write;
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/mutable/keep-accessory.log")
    {
        let _ = writeln!(f, "[{ts}] keep-accessory: {event}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_zero_for_same_point() {
        assert!(distance_m(37.5, -122.3, 37.5, -122.3) < 0.01);
    }

    #[test]
    fn distance_roughly_111km_per_degree_lat() {
        let d = distance_m(37.0, -122.0, 38.0, -122.0);
        assert!((d - 111_200.0).abs() < 1_000.0, "got {d}m");
    }

    #[test]
    fn hundred_meters_is_inside_a_120m_radius() {
        let d = distance_m(37.0000, -122.0, 37.0009, -122.0);
        assert!(d > 90.0 && d < 110.0, "got {d}m");
        assert!(d <= 120.0);
    }

    // ── decision-core tests (the home→OFF path, verified without a
    //    live test that would power the Pi off) ──

    // decide_desired(is_home, parked, archive_active, archive_seen_active,
    //                grace_expired, charge_hold, charge_complete_grace, charge_done)
    #[test]
    fn away_is_always_on() {
        // Away → ON regardless of parked / archive / grace.
        assert_eq!(decide_desired(false, false, false, false, false, false, false, false), Some((true, "away from home")));
        assert_eq!(decide_desired(false, true, true, true, true, false, false, false).map(|d| d.0), Some(true));
    }

    #[test]
    fn home_but_moving_is_untouched() {
        assert_eq!(decide_desired(true, false, false, false, false, false, false, false), None);
    }

    #[test]
    fn home_parked_archiving_holds_on() {
        assert_eq!(decide_desired(true, true, true, false, false, false, false, false).map(|d| d.0), Some(true));
    }

    #[test]
    fn home_parked_archive_finished_turns_off() {
        // Saw an archive run (archive_seen_active), now idle → OFF.
        assert_eq!(decide_desired(true, true, false, true, false, false, false, false).map(|d| d.0), Some(false));
    }

    #[test]
    fn home_parked_within_grace_holds_on() {
        // No archive yet, grace not expired → hold ON, wait for it.
        assert_eq!(decide_desired(true, true, false, false, false, false, false, false).map(|d| d.0), Some(true));
    }

    #[test]
    fn home_parked_grace_expired_nothing_to_archive_turns_off() {
        assert_eq!(decide_desired(true, true, false, false, true, false, false, false).map(|d| d.0), Some(false));
    }

    // ── charge-hold (opt-in) tests ──

    #[test]
    fn charge_hold_overrides_archive_finished_release() {
        // Archive done (would normally release), but plugged in waiting for a
        // charge → hold ON.
        assert_eq!(
            decide_desired(true, true, false, true, true, true, false, false),
            Some((true, "home, plugged in — holding for charge"))
        );
    }

    #[test]
    fn charge_complete_grace_holds_then_done_releases() {
        // Within the post-complete grace → ON; after it (charge_done) → OFF.
        assert_eq!(decide_desired(true, true, false, true, true, false, true, false).map(|d| d.0), Some(true));
        assert_eq!(decide_desired(true, true, false, true, true, false, false, true).map(|d| d.0), Some(false));
    }

    #[test]
    fn active_archive_beats_charge_hold() {
        // An in-progress archive still wins (archive_active first).
        assert_eq!(decide_desired(true, true, true, false, false, true, false, false).map(|d| d.0), Some(true));
    }
}

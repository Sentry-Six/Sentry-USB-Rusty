//! Automatic stable updates admitted only by a successful archive cycle.
use anyhow::{Context, Result, ensure};
use axum::{extract::{State, ConnectInfo, Query}, http::StatusCode, Json};
use serde::Deserialize;
use std::{net::SocketAddr, path::{Path, PathBuf}, time::Duration};
use crate::{router::AppState, update_attempt::{self, Attempt}};

#[path = "auto_update_install.rs"]
mod install;

#[derive(Clone, Deserialize)]
pub(crate) struct Cycle {
    cycle_id: String,
    boot_id: String,
    eligible: bool,
    newly_archived_files: u64,
}

fn read_cycle() -> Result<Cycle> {
    Ok(serde_json::from_slice(&std::fs::read("/tmp/sentryusb-auto-update-cycle.json")?)?)
}
fn prefs(state: &AppState) -> Result<serde_json::Map<String, serde_json::Value>> {
    crate::preferences::load_prefs_checked(&state.drives.store)
}
fn enabled(state: &AppState) -> Result<bool> {
    Ok(prefs(state)?.get("auto_install_stable_updates").and_then(|v| v.as_str()) == Some("enabled"))
}
fn valid_cycle(c: &Cycle) -> bool {
    let control = sentryusb_drives::archive_control::ArchiveControl::default();
    crate::update::read_boot_id().as_deref() == Some(c.boot_id.as_str()) &&
        control.active_cycle().as_deref() == Some(c.cycle_id.as_str()) && !control.cycle_cancelled(&c.cycle_id)
}

/// Only a live Park response may admit an update on a paired vehicle.
/// The future is deliberately not polled for unpaired installations.
async fn final_drive_check(paired: bool, query: impl std::future::Future<Output = Result<String>>) -> Result<()> {
    if paired {
        let gear = query.await.context("Could not confirm vehicle is parked; skipping update until next archive")?;
        ensure!(gear.trim() == "P", "Vehicle is not confirmed in Park; skipping update until next archive");
    }
    Ok(())
}

fn sentry_keep_awake(config: &sentryusb_config::SetupConfig) -> bool {
    let set = |key: &str| config.get(key).is_some_and(|v| !v.trim().is_empty());
    matches!(config.get("SENTRY_CASE").map(String::as_str), Some("1" | "2")) &&
        (set("TESLAFI_API_TOKEN") || set("TESSIE_API_TOKEN") || set("KEEP_AWAKE_WEBHOOK_URL") ||
         (set("TESLA_BLE_VIN") && config.get("BLE_KEEP_AWAKE_ENABLED").is_some_and(|v| matches!(v.as_str(), "yes" | "true" | "1"))))
}

async fn final_sentry_check(paired: bool, sentry_keep_awake: bool, query: impl std::future::Future<Output = Result<String>>) -> Result<()> {
    if paired && !sentry_keep_awake {
        let state = query.await.context("Could not confirm Sentry Mode is off; skipping update until next archive")?;
        ensure!(state.trim() == "Off", "Sentry Mode is not confirmed off; skipping update until next archive");
    }
    Ok(())
}

async fn final_install_gate(
    paired: bool, sentry_keep_awake: bool,
    stop_keep_awake: impl std::future::Future<Output = Result<()>>,
    sentry: impl std::future::Future<Output = Result<String>>,
    gear: impl std::future::Future<Output = Result<String>>,
    archive: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    // A successful reboot never returns to archiveloop's awake_stop. Perform
    // its normal teardown now, while no installed files have been replaced.
    stop_keep_awake.await?;
    final_sentry_check(paired, sentry_keep_awake, sentry).await?;
    // Gear follows the Sentry query; reachability follows both BLE requests.
    final_drive_check(paired, gear).await?;
    archive.await
}

fn checked_awake_stop_script(marker: &Path) -> String {
    format!(r#"{}
export AUTO_UPDATE_STOP_FAILURE={}
function log {{
  case "$*" in
    *[Ff]ail*|*[Ee]rror*|*timed\ out*) touch "$AUTO_UPDATE_STOP_FAILURE" || exit 1 ;;
  esac
  printf '%s: %s\n' "$(date)" "$*" >> "${{LOG_FILE:-/mutable/archiveloop.log}}" 2>/dev/null || true
}}
export -f log
/root/bin/awake_stop
result=$?
[ "$result" = 0 ] && [ ! -e "$AUTO_UPDATE_STOP_FAILURE" ]
"#, crate::drives_handler::AWAKE_PREAMBLE, crate::drives_handler::shell_quote(&marker.to_string_lossy()))
}

pub(crate) async fn recheck(state: &AppState, c: &Cycle) -> Result<()> {
    ensure!(enabled(state)? && valid_cycle(c) && c.eligible && c.newly_archived_files > 0,
        "Archive cycle no longer eligible for automatic update");
    ensure!(!Path::new(&format!("/tmp/archive-stage-failed-{}", c.cycle_id)).exists(), "Archive stage failed");
    ensure!(!state.drives.processor.is_running() && !state.drives.importing.load(std::sync::atomic::Ordering::SeqCst), "Drive work is active");
    let (config, _) = sentryusb_config::parse_file(sentryusb_config::find_config_path())?;
    ensure!(!config.get("TRAVEL_MODE_ENABLED").is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true"|"yes"|"1"|"on")), "Travel Mode active");
    if Path::new("/root/.ble/paired").try_exists()? && let Ok(gate) = std::fs::read_to_string("/mutable/sentryusb-ble-gate.txt") {
        ensure!(!gate.lines().any(|l| matches!(l.trim(), "shift_state=Drive"|"shift_state=Reverse"|"shift_state=Neutral"|"shift_state=D"|"shift_state=R"|"shift_state=N")), "Vehicle is not parked");
    }
    let cloud = state.cloud.uploader.status().await;
    ensure!(cloud.credentials_load_error.is_none(), "Cloud credentials could not be checked");
    ensure!(!cloud.paired || cloud.pending_route_count == 0, "Cloud upload is incomplete");
    if cloud.paired {
        ensure!(!cloud.mutable_sync.running && !cloud.mutable_sync.home_pending &&
            cloud.mutable_sync.failed_stages.is_empty(), "Cloud synchronization is incomplete");
        let pending = cloud.mutable_sync.pending_edits.as_ref().context("Cloud pending changes could not be checked")?;
        ensure!(pending.drive_tags == 0 && pending.charging == 0 && pending.rates == 0, "Cloud changes remain pending");
        ensure!(cloud.last_upload_error.is_none(), "Cloud reported an upload error");
    }
    sentryusb_shell::run_with_timeout(Duration::from_secs(15), "bash", &["-c",
        ARCHIVE_REACHABILITY_PROBE])
        .await.context("Archive is no longer reachable")?;
    ensure!(valid_cycle(c), "Archive cancelled during reachability check");
    Ok(())
}

const ARCHIVE_REACHABILITY_PROBE: &str = "source /root/bin/envsetup.sh && if [ -z \"${ARCHIVE_SERVER:-}\" ]; then if [ -n \"${RSYNC_SERVER:-}\" ]; then ARCHIVE_SERVER=$RSYNC_SERVER; elif [ -n \"${RCLONE_DRIVE:-}\" ]; then ARCHIVE_SERVER=8.8.8.8; fi; fi; test -n \"${ARCHIVE_SERVER:-}\" && /root/bin/archive-is-reachable.sh \"$ARCHIVE_SERVER\"";

async fn latest_stable(current: &str) -> Result<Option<String>> {
    let v: serde_json::Value = crate::http_client().get(format!(
        "https://api.github.com/repos/{}/releases/latest", crate::update::update_repo()))
        .header("User-Agent", "sentryusb-auto-update").timeout(Duration::from_secs(15))
        .send().await?.error_for_status()?.json().await?;
    let tag = v.get("tag_name").and_then(|v| v.as_str()).context("Release has no tag")?;
    let draft = v.get("draft").and_then(|v| v.as_bool()).context("Release draft flag missing")?;
    let pre = v.get("prerelease").and_then(|v| v.as_bool()).context("Release prerelease flag missing")?;
    ensure!(numeric_version(current).is_some() && numeric_version(tag).is_some(), "Unrecognized version format");
    Ok(stable_target_is_newer(current, tag, draft, pre).then(|| tag.to_owned()))
}

async fn available_notification(state: &AppState) -> Result<()> {
    let settings = prefs(state)?;
    if settings.get("auto_update_check").and_then(|v| v.as_str()) == Some("disabled") { return Ok(()); }
    let (_, Json(v)) = crate::update::check_for_update(State(state.clone()), Query(Default::default())).await;
    let release = if v["stable"]["available"] == true { &v["stable"] }
        else if settings.get("update_channel").and_then(|v| v.as_str()) == Some("prerelease") && v["prerelease"]["available"] == true { &v["prerelease"] } else { return Ok(()); };
    let Some(tag) = release["version"].as_str().filter(|v| numeric_version(v).is_some()) else { return Ok(()); };
    let marker = format!("/tmp/sentryusb-update-notified-{tag}");
    if Path::new(&marker).exists() { return Ok(()); }
    let text = format!("Update available: {tag}. Open Settings to install.");
    if let Some(out) = crate::notifications::dispatch_and_record("SentryUSB:", &text, Some("update"), Some("info"), None, Some(&text)).await {
        if out.failures.is_empty() { std::fs::write(marker, b"")?; }
    }
    Ok(())
}

/// Local shell handoff. No HTTP caller can supply its own success evidence.
pub async fn after_archive(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> (StatusCode, Json<serde_json::Value>) {
    if !peer.ip().to_canonical().is_loopback() { return crate::json_error(StatusCode::FORBIDDEN, "Local archive coordinator only"); }
    // Own the task even if the HTTP connection goes away during installation.
    match tokio::spawn(async move { coordinate(&state).await }).await {
        Ok(Ok(())) => crate::json_ok(),
        Ok(Err(e)) => crate::json_error(StatusCode::CONFLICT, &format!("{e:#}")),
        Err(_) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Update task interrupted"),
    }
}

async fn coordinate(state: &AppState) -> Result<()> {
    ensure!(STARTUP_READY.load(std::sync::atomic::Ordering::SeqCst), "Startup update verification is not finished");
    let c = read_cycle()?;
    ensure!(valid_cycle(&c), "Archive cycle ended or cancelled");
    let _guard = crate::update::acquire_update().await?;
    if !claim_cycle(Path::new("/tmp"), &c.cycle_id)? { return Ok(()); }
    if !enabled(state)? || !c.eligible || c.newly_archived_files == 0 {
        return available_notification(state).await;
    }
    if recheck(state, &c).await.is_err() { return available_notification(state).await; }
    let recorded_version = crate::update::read_current_version();
    let previous = update_attempt::load(&update_attempt::current_path())?;
    if let Some(previous) = &previous {
        ensure!(previous.terminal(), "An interrupted update requires startup recovery before retry");
    }
    let current = comparison_version(&recorded_version, previous.as_ref()).to_owned();
    let mut attempt = Attempt::new(&c.boot_id, &c.cycle_id, &current, "");
    let result = async {
        let Some(target) = latest_stable(&current).await? else { return Ok(()); };
        attempt.target = target;
        update_attempt::save(&update_attempt::current_path(), &attempt)?;
        let staged = tokio::select! {
            staged = install::stage(&state.hub, &attempt) => staged?,
            stopped = monitor_cycle(state, &c) => { stopped?; unreachable!() },
        };
        recheck(state, &c).await?;
        let _work = sentryusb_drives::archive_control::UpdateWorkGuard::try_acquire().context("Background work is still active")?;
        update_attempt::save(&update_attempt::directory().join(format!("{}.start.json", attempt.id)), &attempt)?;
        deliver_pending().await;
        // Query the car after downloads and provider delivery, never use a
        // cached Park sample as authorization. Recheck reachability afterward
        // too: the car may leave while a slow BLE query is completing.
        let paired = Path::new("/root/.ble/paired").try_exists()?;
        let (config, _) = sentryusb_config::parse_file(sentryusb_config::find_config_path())?;
        final_install_gate(paired, sentry_keep_awake(&config), async {
            let script = checked_awake_stop_script(Path::new(&format!("/tmp/archive-stage-failed-{}", c.cycle_id)));
            sentryusb_shell::run_with_timeout(Duration::from_secs(120), "bash", &["-c", &script])
                .await.context("Could not finish archive keep-awake cleanup")?;
            Ok(())
        }, async {
            sentryusb_shell::run_with_timeout(Duration::from_secs(65),
                "/root/bin/sentryusb-ble-action", &["sentry-state"]).await
        }, async {
            sentryusb_shell::run_with_timeout(Duration::from_secs(65),
                "/root/bin/sentryusb-ble-action", &["drive-state"]).await
        }, recheck(state, &c)).await?;
        let _cycle_lock = tokio::task::spawn_blocking(|| sentryusb_drives::archive_mount_lock::acquire_path(
            Path::new("/tmp/archive-cycle.lock"), Duration::from_secs(5))).await??;
        ensure!(valid_cycle(&c), "Archive cancellation accepted before installation");
        install::commit(&staged, &mut attempt).await?;
        install::arm_shutdown_receipt(&attempt).await?;
        // No automatic replay after power loss; this phase means all required
        // files were installed and the expected reboot has been requested.
        attempt.phase = "awaiting_reboot".into();
        update_attempt::save(&update_attempt::current_path(), &attempt)?;
        state.hub.broadcast("update_status", &serde_json::json!({"status":"restarting"}));
        sentryusb_shell::run("reboot", &[]).await?;
        // Keep admission closed until shutdown; if reboot never happens, report
        // failure instead of leaving an everlasting in-progress state.
        tokio::time::sleep(Duration::from_secs(90)).await;
        anyhow::bail!("Device did not reboot after update")
    }.await;
    if let Err(e) = result {
        if attempt.phase == "preparing" {
            if let Err(cleanup) = install::cleanup_uninstalled(Path::new("/opt/sentryusb"), &attempt.id) {
                tracing::warn!("Could not remove incomplete update staging: {cleanup:#}");
            }
            for component in ["sentryusb-tesla-telemetry", "sentryusb-ble-action"] {
                let _ = std::fs::remove_file(format!("/root/bin/.{component}-{}", attempt.id));
            }
        }
        attempt.phase = "failed".into();
        attempt.detail = sentryusb_notify::safe_provider_error(&e, &[]);
        update_attempt::save(&update_attempt::current_path(), &attempt)?;
        queue_outcome(&attempt)?;
        deliver_pending().await;
        return Err(e);
    }
    if attempt.target.is_empty() { available_notification(state).await?; }
    Ok(())
}

fn comparison_version<'a>(recorded: &'a str, previous: Option<&'a Attempt>) -> &'a str {
    // A version file can advance before reboot/migration. An unsuccessful
    // attempt has not established that version as a completed installation.
    match previous {
        Some(a) if a.phase != "verified" && a.target == recorded => &a.source,
        _ => recorded,
    }
}

fn claim_cycle(directory: &Path, id: &str) -> Result<bool> {
    ensure!(id.len() <= 128 && !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-')), "Invalid cycle ID");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join(format!("sentryusb-auto-update-attempted-{id}"))) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

async fn monitor_cycle(state: &AppState, c: &Cycle) -> Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        recheck(state, c).await?;
    }
}

fn outcome_path(a: &Attempt) -> PathBuf { update_attempt::directory().join(format!("{}.outcome.json", a.id)) }
fn queue_outcome(a: &Attempt) -> Result<()> {
    let p = outcome_path(a);
    if update_attempt::load(&p)?.is_none() { update_attempt::save(&p, a)?; }
    Ok(())
}

static STARTUP_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn prepare_startup_verification() -> Result<Attempt> {
    let path = update_attempt::current_path();
    let a = update_attempt::load(&path)?.context("Missing update receipt")?;
    ensure!(!a.binary_sha256.is_empty() && install::sha256(Path::new("/proc/self/exe"))? == a.binary_sha256,
        "Running executable does not match staged update");
    update_attempt::begin_verification(&path, &crate::update::read_boot_id().context("Boot ID unavailable")?)
}
pub async fn recover_after_startup(migration_ok: bool) {
    let result = (|| -> Result<()> {
        let Some(mut a) = update_attempt::load(&update_attempt::current_path())? else { return Ok(()); };
        if !a.terminal() {
            let boot = crate::update::read_boot_id().context("Boot ID unavailable")?;
            let matches = !a.binary_sha256.is_empty() && install::sha256(Path::new("/proc/self/exe")).ok().as_deref() == Some(a.binary_sha256.as_str());
            a.phase = a.boot_outcome(&boot, matches, migration_ok).into();
            a.detail = format!("Update from {} to {}; startup outcome: {}", a.source, a.target, a.phase);
            update_attempt::save(&update_attempt::current_path(), &a)?;
        }
        if a.phase == "verified" {
            if let Err(e) = install::cleanup_uninstalled(Path::new("/opt/sentryusb"), &a.id) {
                tracing::warn!("Could not remove verified update staging: {e:#}");
            }
        }
        queue_outcome(&a)
    })();
    if let Err(e) = result { tracing::error!("Auto-update recovery: {e:#}"); }
    STARTUP_READY.store(true, std::sync::atomic::Ordering::SeqCst);
    loop {
        deliver_pending().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

static DELIVERY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn start_notice_is_current(notice: &Attempt, current: Option<&Attempt>) -> bool {
    current.is_some_and(|a| a.id == notice.id && !a.terminal())
}
async fn deliver_pending() {
    let _guard = DELIVERY_LOCK.lock().await;
    if !crate::notification_center::is_type_enabled(Some("update")) { return; }
    let Ok(entries) = std::fs::read_dir(update_attempt::directory()) else { return; };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        let is_start = name.ends_with(".start.json");
        if !is_start && !name.ends_with(".outcome.json") { continue; }
        let Ok(Some(mut a)) = update_attempt::load(&p) else { continue; };
        if is_start {
            let Ok(current) = update_attempt::load(&update_attempt::current_path()) else { continue; };
            if !start_notice_is_current(&a, current.as_ref()) {
                let _ = std::fs::remove_file(&p);
                continue;
            }
        }
        let summary = if is_start { format!("Update {} available. Installing now.", a.target) } else { a.summary() };
        let config = sentryusb_notify::NotifyConfig::from_config();
        let delivered: Vec<_> = a.delivered.iter().cloned().collect();
        let results = sentryusb_notify::send_to_selected(&config, &sentryusb_notify::NotifyRequest {
            title: "SentryUSB:", message: &summary, type_hint: Some(if is_start || a.phase == "verified" { "info" } else { "error" }), notification_type: Some("update"), archive_total_count: None,
        }, &delivered).await;
        let mut errors = std::collections::HashMap::new();
        for (provider, result) in results {
            match result { Ok(()) => { a.delivered.insert(provider); }, Err(e) => { errors.insert(provider, e.to_string()); } }
        }
        let mut statuses: std::collections::HashMap<String, String> = a.delivered.iter().map(|p| (p.clone(), "ok".into())).collect();
        statuses.extend(errors.keys().map(|p| (p.clone(), "error".into())));
        let event = crate::notification_center::NotificationEvent {
            id: if is_start { format!("{}-start", a.id) } else { a.id.clone() }, timestamp: a.timestamp, event_type: "update".into(), title: "SentryUSB:".into(),
            message: format!("{summary}\n{}", a.detail), summary: Some(summary),
            providers: statuses.keys().cloned().collect(), results: statuses, provider_errors: errors,
            ..Default::default()
        };
        if !a.history_recorded || !event.provider_errors.is_empty() || a.delivered.len() != delivered.len() {
            a.history_recorded = crate::notification_center::record_update_event(event).is_ok();
            if let Err(e) = update_attempt::save(&p, &a) { tracing::error!("Update notification receipt: {e:#}"); }
        }
    }
}

fn numeric_version(value: &str) -> Option<[u64; 3]> {
    let parts: Vec<_> = value.trim().strip_prefix('v').unwrap_or(value.trim()).split('.').collect();
    if parts.len() != 3 { return None; }
    let mut version = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) { return None; }
        version[i] = part.parse().ok()?;
    }
    Some(version)
}

fn stable_target_is_newer(installed: &str, candidate: &str, draft: bool, prerelease: bool) -> bool {
    if draft || prerelease { return false; }
    match (numeric_version(installed), numeric_version(candidate)) {
        (Some(current), Some(target)) => target > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_install_notice_expires_when_attempt_finishes_or_is_superseded() {
        let mut a = Attempt::new("boot", "cycle", "v1.0.0", "v2.0.0");
        assert!(start_notice_is_current(&a, Some(&a)));
        assert!(!start_notice_is_current(&a, None));
        let mut newer = a.clone(); newer.id.push_str("-next");
        assert!(!start_notice_is_current(&a, Some(&newer)));
        for phase in ["verified", "failed", "interrupted"] {
            a.phase = phase.into();
            assert!(!start_notice_is_current(&a, Some(&a)), "{phase}");
        }
    }
    #[test]
    fn successful_exit_cannot_hide_keep_awake_teardown_failure() {
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("failure");
        for (message, expected) in [("Tesla BLE: Sentry Mode disabled", true), ("Tesla BLE: Failed to disable Sentry Mode", false), ("Tessie: API returned failure", false)] {
            let _ = std::fs::remove_file(&marker);
            let script = checked_awake_stop_script(&marker)
                .replace(crate::drives_handler::AWAKE_PREAMBLE, "")
                .replace("/root/bin/awake_stop", &format!("bash -c {}", crate::drives_handler::shell_quote(&format!("log {}; exit 0", crate::drives_handler::shell_quote(message)))));
            let status = std::process::Command::new("bash").args(["-c", &script])
                .env("LOG_FILE", d.path().join("log")).status().unwrap();
            assert_eq!(status.success(), expected, "{message}");
        }
    }
    #[tokio::test]
    async fn final_gate_stops_keep_awake_before_live_checks_and_aborts_on_each_failure() {
        use std::{cell::RefCell, rc::Rc};
        for fail_at in ["none", "stop", "sentry", "gear", "archive"] {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let result = final_install_gate(true, false,
                async { calls.borrow_mut().push("stop"); ensure!(fail_at != "stop", "stop failed"); Ok(()) },
                async { calls.borrow_mut().push("sentry"); Ok(if fail_at == "sentry" { "On" } else { "Off" }.into()) },
                async { calls.borrow_mut().push("gear"); Ok(if fail_at == "gear" { "D" } else { "P" }.into()) },
                async { calls.borrow_mut().push("archive"); ensure!(fail_at != "archive", "archive lost"); Ok(()) },
            ).await;
            assert_eq!(result.is_ok(), fail_at == "none");
            let expected = match fail_at { "stop" => 1, "sentry" => 2, "gear" => 3, _ => 4 };
            assert_eq!(&*calls.borrow(), &["stop", "sentry", "gear", "archive"][..expected]);
        }
        final_install_gate(false, false, async { Ok(()) }, async { panic!("unpaired Sentry query") },
            async { panic!("unpaired gear query") }, async { Ok(()) }).await.unwrap();
    }
    #[tokio::test]
    async fn final_ble_check_requires_live_park_when_paired() {
        for state in ["P", "P\n", "D", "R", "N", "", "Unknown", "Park\nD"] {
            let result = final_drive_check(true, async { Ok(state.to_owned()) }).await;
            assert_eq!(result.is_ok(), state.trim() == "P", "state {state:?}");
        }
        assert!(final_drive_check(true, async { anyhow::bail!("BLE unavailable") }).await.is_err());
    }

    #[tokio::test]
    async fn unpaired_install_does_not_attempt_a_ble_query() {
        final_drive_check(false, async { panic!("must not query an unpaired car") }).await.unwrap();
    }
    #[tokio::test]
    async fn sentry_requires_off_except_for_configured_sentry_keep_awake() {
        for state in ["Off", "Off\n", "On", "Unknown", ""] {
            assert_eq!(final_sentry_check(true, false, async { Ok(state.into()) }).await.is_ok(), state.trim() == "Off");
        }
        assert!(final_sentry_check(true, false, async { anyhow::bail!("query failed") }).await.is_err());
        final_sentry_check(false, false, async { panic!("unpaired") }).await.unwrap();
        final_sentry_check(true, true, async { panic!("Sentry keep-awake exception") }).await.unwrap();
        let mut config = sentryusb_config::SetupConfig::new();
        config.insert("SENTRY_CASE".into(), "1".into());
        assert!(!sentry_keep_awake(&config), "mode without a provider is insufficient");
        config.insert("TESLA_BLE_VIN".into(), "test-vin".into());
        assert!(!sentry_keep_awake(&config), "disabled BLE keep-awake is insufficient");
        config.insert("BLE_KEEP_AWAKE_ENABLED".into(), "yes".into());
        assert!(sentry_keep_awake(&config));
        config.insert("SENTRY_CASE".into(), "2".into());
        assert!(sentry_keep_awake(&config));
        config.insert("SENTRY_CASE".into(), "3".into());
        assert!(!sentry_keep_awake(&config), "nudge mode cannot bypass Sentry check");
        for provider in ["TESLAFI_API_TOKEN", "TESSIE_API_TOKEN", "KEEP_AWAKE_WEBHOOK_URL"] {
            let mut config = sentryusb_config::SetupConfig::new();
            config.insert("SENTRY_CASE".into(), "1".into());
            config.insert(provider.into(), "fixture".into());
            assert!(sentry_keep_awake(&config));
        }
    }
    #[test]
    fn reachability_uses_archive_loop_targets_for_smb_rsync_and_rclone() {
        let script = ARCHIVE_REACHABILITY_PROBE.replace("source /root/bin/envsetup.sh", ":")
            .replace("/root/bin/archive-is-reachable.sh", "printf '%s'");
        for (server, rsync, rclone, expected) in [
            ("nas", "", "", "nas"), ("", "rsync-host", "", "rsync-host"),
            ("", "", "remote", "8.8.8.8"), ("", "", "", ""),
        ] {
            let output = std::process::Command::new("bash").args(["-c", &script])
                .env("ARCHIVE_SERVER", server).env("RSYNC_SERVER", rsync).env("RCLONE_DRIVE", rclone)
                .output().unwrap();
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            assert_eq!(output.status.success(), !expected.is_empty());
        }
    }
    #[test]
    fn stable_policy_uses_numeric_version_and_github_flags() {
        for (current, candidate, draft, pre, expected) in [
            ("v3.10.22", "v4.0.0", false, false, true),
            ("v3.10.22", "v3.9.9", false, false, false),
            ("v3.10.22", "v3.10.22", false, false, false),
            ("v3.10.22", "v4.0.0", false, true, false),
            ("v3.10.22", "v4.0.0", true, false, false),
            ("dev", "v4.0.0", false, false, false),
            ("v3.10.22", "v4.0.0.1", false, false, false),
            ("v3.10.22", "v4.0.0-beta", false, false, false),
        ] {
            assert_eq!(stable_target_is_newer(current, candidate, draft, pre), expected,
                "{current} -> {candidate}, draft={draft}, prerelease={pre}");
        }
    }
    #[test]
    fn next_cycle_can_retry_immediately_but_the_same_cycle_cannot() {
        let d = tempfile::tempdir().unwrap();
        assert!(claim_cycle(d.path(), "123:first").unwrap());
        assert!(!claim_cycle(d.path(), "123:first").unwrap());
        assert!(claim_cycle(d.path(), "123:next").unwrap());
        assert!(claim_cycle(d.path(), "../outside").is_err());
    }
    #[test]
    fn failed_install_version_marker_does_not_suppress_next_archive_retry() {
        let mut a = Attempt::new("old", "cycle", "v3.10.22", "v4.0.0");
        a.phase = "failed".into();
        assert_eq!(comparison_version("v4.0.0", Some(&a)), "v3.10.22");
        assert!(stable_target_is_newer(comparison_version("v4.0.0", Some(&a)), "v4.0.0", false, false));
        a.phase = "verified".into();
        assert_eq!(comparison_version("v4.0.0", Some(&a)), "v4.0.0");
        assert!(!stable_target_is_newer(comparison_version("v4.0.0", Some(&a)), "v4.0.0", false, false));
    }
}

//! In-app XFS `backingfiles` storage repair.
//!
//! Recovers the external-SSD XFS partition behind `/backingfiles` when it
//! won't mount — CRC / dirty-log corruption, typically after a power loss
//! (see `docs/storage-repair-handoff.md` for the incident this mirrors).
//! It reproduces the manual `xfs_repair` recovery a human would run over SSH,
//! but guard-railed for the web UI:
//!
//!   * The real backing device is resolved at runtime (never the hard-coded
//!     `/dev/sda2`), and the repair refuses to touch the root disk.
//!   * It is only offered for a dedicated XFS data partition that is not the
//!     root, boot, mutable or swap device — on an external drive or on the
//!     SD card itself (`repairable` in the health response gates the card).
//!   * It only runs while the server is in database-free degraded mode, so no
//!     database handle can keep the drive busy, and it holds the same slot
//!     the degraded-mode reopen probe and USB enable take.
//!   * It NEVER stops the `sentryusb` service: that service is the web server
//!     running this very handler. Only the archive loop and the USB gadget are
//!     quiesced; the UI keeps serving from the (separate) root card.
//!   * The non-destructive path runs automatically (`xfs_repair -n`, plain
//!     `xfs_repair`, and a mount-to-replay-log retry). It HARD STOPS before the
//!     destructive `xfs_repair -L` — that only runs when the user explicitly
//!     re-submits with `confirm_destructive: true`.
//!   * On success it verifies the mounted drive and its disk images, then
//!     reboots the Pi after a short countdown: the normal boot path brings
//!     storage, the database, archiving and the USB drive back together.
//!   * Its outcome is saved on /mutable, so "the destructive step is needed"
//!     survives a page refresh or a service restart.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::process::Command;


const BACKINGFILES: &str = "/backingfiles";
/// XFS label the setup code stamps on the backingfiles partition
/// (`mkfs.xfs -L backingfiles`); used to find the device when it isn't
/// mounted (the corruption case).
const XFS_LABEL: &str = "backingfiles";
/// Bind/autofs mount points archiveloop exposes from inside `/backingfiles`.
/// All must be released before the device can be unmounted.
const SUBMOUNTS: &[&str] = &["/mnt/cam", "/mnt/music", "/mnt/lightshow", "/mnt/boombox"];
/// Writable partition where the human-readable repair transcript is kept.
const REPAIR_LOG_DIR: &str = "/mutable";
/// Per-command ceiling. `xfs_repair` on a large multi-TB SSD can take minutes;
/// 5 min matches the wizard's xfs_repair budget.
const CMD_TIMEOUT: Duration = Duration::from_secs(300);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const REPAIR_TOOL_SELECTOR: &str = include_str!("../../../run/xfs-repair-tool.py");
// Shared by HTTP and boot recovery, including discovery/mount retry/preflight.
// Move the permit into the task so cancellation of the HTTP request cannot
// release admission while a repair is still running.
static REPAIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Set once the server decides to run without a database (degraded mode).
/// Repair unmounts the footage drive, which a live database would hold busy.
static DATABASE_FREE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called by the server when it enters degraded mode.
pub fn set_database_free() {
    DATABASE_FREE.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn database_free() -> bool {
    DATABASE_FREE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Take the repair slot without running a repair, so the caller's work
/// can't overlap one (the degraded-mode reopen probe, USB enable). `None`
/// while a repair is running.
pub fn try_pause_repairs() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    REPAIR_LOCK.try_lock().ok()
}

/// Any xfs_repair process, including the bundled one started through the
/// dynamic loader (its command line ends in `.../xfs_repair ARGS`).
async fn xfs_repair_running() -> bool {
    run_capture(PROBE_TIMEOUT, "pgrep", &["-f", "(^|/)xfs_repair( |$)"]).await.ok
}

/// Seconds between "repair succeeded" and the automatic reboot.
const REBOOT_COUNTDOWN: Duration = Duration::from_secs(15);
const TELEMETRY_UNIT: &str = "sentryusb-telemetry";
const ARCHIVE_UNIT: &str = "sentryusb-archive";

/// Selected once per repair, before stopping services. Every stage (including
/// destructive escalation) uses this exact executable/loader combination.
struct RepairTool(Vec<String>);

impl RepairTool {
    fn invocation(&self, args: &[&str]) -> (String, Vec<String>) {
        let mut command_args = self.0[1..].to_vec();
        command_args.extend(args.iter().map(|arg| (*arg).to_string()));
        (self.0[0].clone(), command_args)
    }

    async fn run(&self, timeout: Duration, args: &[&str]) -> CmdResult {
        let (program, arguments) = self.invocation(args);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        run_capture(timeout, &program, &arguments).await
    }
}

async fn select_repair_tool(selector: &str) -> Result<RepairTool, String> {
    let prepared = run_capture(CMD_TIMEOUT, "python3", &["-c", selector]).await;
    if !prepared.ok {
        return Err(prepared.output);
    }
    if !prepared.stderr.trim().is_empty() {
        tracing::warn!("[storage-repair] tool preparation diagnostics: {}", prepared.stderr.trim());
    }
    let command: Vec<String> = serde_json::from_str(&prepared.stdout)
        .map_err(|e| format!("Invalid XFS tool selection: {e}"))?;
    if command.is_empty() || !command[0].starts_with('/') {
        return Err("XFS tool selector returned no absolute executable path".into());
    }
    Ok(RepairTool(command))
}

// ───────────────────────── command capture ─────────────────────────

/// Outcome of a spawned command with its combined output preserved
/// regardless of exit status.
struct CmdResult {
    ok: bool,
    code: i32,
    output: String,
    stdout: String,
    stderr: String,
}

/// Run a command capturing exit status + combined stdout/stderr, never
/// erroring on a non-zero exit.
///
/// `sentryusb_shell::run` bails on any non-zero exit — unusable here because
/// `xfs_repair -n` exits non-zero *precisely when it finds damage*, which is
/// exactly the signal we need to read.
async fn run_capture(timeout: Duration, name: &str, args: &[&str]) -> CmdResult {
    let fut = Command::new(name).args(args).kill_on_drop(true).output();
    match tokio::time::timeout(timeout, fut).await {
        Err(_) => CmdResult {
            ok: false,
            code: -1,
            output: format!("(timed out after {timeout:?})"),
            stdout: String::new(),
            stderr: String::new(),
        },
        Ok(Err(e)) => CmdResult {
            ok: false,
            code: -1,
            output: format!("(failed to spawn {name}: {e})"),
            stdout: String::new(),
            stderr: String::new(),
        },
        Ok(Ok(o)) => {
            let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
            let mut s = stdout.clone();
            let err = &stderr;
            if !err.trim().is_empty() {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(err);
            }
            CmdResult {
                ok: o.status.success(),
                code: o.status.code().unwrap_or(-1),
                output: s.trim_end().to_string(),
                stdout,
                stderr,
            }
        }
    }
}

// ───────────────────────── pure helpers (unit-tested) ─────────────────────────

/// Strip the partition suffix from a `/dev` name to get the parent disk.
/// `sda2` → `sda`, `mmcblk0p2` → `mmcblk0`, `nvme0n1p3` → `nvme0n1`.
/// Mirrors the parent-disk logic in [`crate::devices`].
fn parent_disk(dev: &str) -> String {
    let d = dev.strip_prefix("/dev/").unwrap_or(dev);
    if d.contains("mmcblk") || d.contains("nvme") || d.contains("loop") {
        // p-separated partition suffix, e.g. mmcblk0p2 / nvme0n1p3.
        if let Some(idx) = d.rfind('p') {
            let suffix = &d[idx + 1..];
            if idx > 0 && !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return d[..idx].to_string();
            }
        }
        d.to_string()
    } else {
        // sd-style: partition suffix is trailing digits.
        let trimmed: String = d
            .chars()
            .rev()
            .skip_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if trimmed.is_empty() { d.to_string() } else { trimmed }
    }
}

/// Find the source device for a mountpoint in `/proc/mounts` text.
fn resolve_mount_source(mounts: &str, mountpoint: &str) -> Option<String> {
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(src), Some(mp)) = (f.next(), f.next()) else {
            continue;
        };
        if mp == mountpoint && src.starts_with("/dev/") {
            return Some(src.to_string());
        }
    }
    None
}

/// True when `xfs_repair` output says the dirty log must be replayed (or
/// destroyed with `-L`). These phrases are stable across xfs_repair versions.
fn needs_log_replay(out: &str) -> bool {
    let l = out.to_ascii_lowercase();
    l.contains("destroy the log") || l.contains("metadata changes in a log")
}

/// Marker recording that an auto repair already ran for the current
/// corruption incident. Lives on /mutable (survives reboot + read-only
/// root). Written BEFORE the repair starts so a crash mid-repair can't
/// reboot-loop; cleared only by a boot where /backingfiles is mounted.
const AUTO_REPAIR_MARKER: &str = "/mutable/.storage_auto_repair_attempted";
const PREFLIGHT_NOTICE_MARKER: &str = "/mutable/.storage_auto_repair_preflight_notice";

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Notification throttling is independent of repair eligibility. A Pi without
// a cached tool should retry every boot, without sending a push every drive.
fn claim_preflight_notice(path: &str, now: u64) -> bool {
    let previous = std::fs::read_to_string(path).ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("ts").and_then(|ts| ts.as_u64()));
    if previous.is_some_and(|last| now >= last && now - last < 24 * 60 * 60) {
        return false;
    }
    if let Err(error) = std::fs::write(path, format!("{{\"ts\":{now}}}\n")) {
        tracing::warn!("[storage-boot] could not persist preflight notice limit: {error}");
    }
    true
}

fn marker_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// Durably record a marker. `false` if /mutable isn't its own mounted
/// filesystem or the write failed: the caller must not rely on it.
fn write_marker(path: &str) -> bool {
    let ts = unix_seconds();
    match write_durable(path, format!("{{\"ts\":{ts}}}\n").as_bytes()) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("[storage-boot] failed to write marker {path}: {e:#}");
            false
        }
    }
}

/// Write `path` atomically (temp file, fsync, rename, fsync the directory),
/// only when its parent is a separately mounted filesystem — never onto the
/// SD card root underneath an unmounted /mutable.
fn write_durable(path: &str, data: &[u8]) -> anyhow::Result<()> {
    let dir = std::path::Path::new(path).parent().ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
    let dir = dir.to_string_lossy();
    if !sentryusb_drives::storage::is_separate_mount(&dir) {
        anyhow::bail!("{dir} is not mounted");
    }
    write_atomic(path, data)
}

/// Temp file, fsync, rename, fsync the directory.
fn write_atomic(path: &str, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let path = std::path::Path::new(path);
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn clear_marker(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// What the boot check should do, given the observed state. Pure —
/// the caller gathers the inputs (including a one-shot mount retry
/// that can flip `mounted` to true).
#[derive(Debug, PartialEq, Eq)]
enum BootAction {
    /// Feature off or storage not eligible — do nothing.
    Skip(&'static str),
    /// Storage healthy — clear any stale incident marker.
    ClearMarker,
    /// Corrupt again after a previous auto attempt — notify, never loop.
    NotifyRepeatCorruption,
    /// First detection of this incident — run the auto repair.
    Repair,
}

fn decide_boot_action(
    auto_enabled: bool,
    device_found: bool,
    repairable: bool,
    mounted: bool,
    marker_present: bool,
) -> BootAction {
    if !auto_enabled {
        return BootAction::Skip("storage_auto_repair disabled");
    }
    if !device_found {
        return BootAction::Skip("no backingfiles device found");
    }
    if !repairable {
        return BootAction::Skip("backingfiles is not a repairable data partition");
    }
    if mounted {
        return BootAction::ClearMarker;
    }
    if marker_present {
        return BootAction::NotifyRepeatCorruption;
    }
    BootAction::Repair
}

/// Policy at the "regular repair finished" gate.
///
/// Interactive keeps the historical semantics: `-L` is offered only when
/// xfs_repair says the dirty log is the blocker, and runs only with the
/// user's explicit confirmation; other failures are terminal errors.
/// Auto mode runs `-L` on ANY failure when the user has pre-authorized
/// it via the force toggle (their explicit product decision), otherwise
/// it stops and asks for a manual force fix.
#[derive(Debug, PartialEq, Eq)]
enum Escalation {
    /// Repair succeeded — continue to verification.
    Proceed,
    /// Stop; a destructive repair is required but not authorized.
    StopNeedsForce,
    /// Run `xfs_repair -L` now.
    RunForce,
    /// Unrepairable without help `-L` can't give (interactive only).
    Fail,
}

fn escalation_action(rep_ok: bool, needs_replay: bool, auto: bool, force_allowed: bool) -> Escalation {
    if rep_ok {
        return Escalation::Proceed;
    }
    if auto || needs_replay {
        if force_allowed { Escalation::RunForce } else { Escalation::StopNeedsForce }
    } else {
        Escalation::Fail
    }
}

/// How a repair run terminates and who authorizes `-L`.
#[derive(Clone, Copy)]
pub(crate) enum RepairMode {
    /// Web-UI flow: WS broadcasts only; hard stop before `-L` unless the
    /// user re-submitted with confirm_destructive; user presses Reboot.
    Interactive { confirm_destructive: bool },
    /// Boot-time flow: pushes notifications, runs `-L` on any regular
    /// failure when pre-authorized, and reboots itself on success.
    AutoBoot { force_allowed: bool },
}

/// Did the plain repair reach a real verdict that clearing the log could
/// change? Automatic mode only clears the log when it could.
fn force_may_help(r: &CmdResult) -> bool {
    if r.ok || r.code <= 0 {
        // Success, a timeout, a spawn failure or a signal (no exit code).
        return false;
    }
    !device_trouble(&r.output)
}

/// Output that points at the disk or the system rather than at the
/// filesystem's metadata: clearing the log can't fix these.
fn device_trouble(output: &str) -> bool {
    let l = output.to_ascii_lowercase();
    l.contains("input/output error")
        || l.contains("i/o error")
        || l.contains("no such device")
        || l.contains("cannot allocate memory")
        || l.contains("out of memory")
        || l.contains("killed")
}

/// The escalation for this run. In automatic mode a failure that clearing
/// the log can't fix (see [`force_may_help`]), or a replay mount that hit
/// device errors, stops at a plain failure even when the log is dirty.
fn decide_escalation(rep: &CmdResult, auto: bool, force_allowed: bool, replay_device_trouble: bool) -> Escalation {
    if auto && !rep.ok && (!force_may_help(rep) || replay_device_trouble) {
        return Escalation::Fail;
    }
    escalation_action(rep.ok, needs_log_replay(&rep.output), auto, force_allowed)
}

/// Shown before the destructive step, and saved with the state.
const MSG_LOG_RESET: &str = "The safe repair couldn't fix the footage drive. The last option resets the XFS log (xfs_repair -L). That discards pending filesystem changes, and the repair can then remove or move files and folders, including the car's disk images, snapshots and the drive database. Losses can go beyond the newest clips, and the repair can still fail after making changes. Confirm only if you accept that.";

// Auto-repair notification copy (spec-fixed wording — do not edit).
const MSG_AUTO_SUCCESS: &str = "Backing-files corruption detected at boot. Automatic repair succeeded — rebooting the Pi now.";
const MSG_NEEDS_MANUAL_FORCE: &str = "Backing-files corruption detected at boot. Automatic repair failed — you must run the force fix manually from Settings → System → Repair Storage.";
const MSG_FORCE_SUCCESS: &str = "Backing-files corruption detected at boot. Force fix succeeded after the regular repair failed — rebooting the Pi now.";
const MSG_HARD_FAIL: &str = "Backing-files corruption detected at boot. Automatic repair FAILED — the drive may be failing. Check the SSD's power, cable and enclosure.";
const MSG_REPEAT_CORRUPTION: &str = "Backing-files corruption detected again after a recent auto repair. Not retrying automatically — the SSD may be failing. Check power/cable/enclosure and run repair manually.";

/// Title for storage-repair push notifications. Mirrors the runtime
/// scripts' `$NOTIFICATION_TITLE` (sentryusb.conf), same fallback as
/// tesla_telemetry's keep-awake failure push.
fn notification_title() -> String {
    let (active, _) = sentryusb_config::parse_file(sentryusb_config::find_config_path())
        .unwrap_or_default();
    active
        .get("NOTIFICATION_TITLE")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "SentryUSB".to_string())
}

/// Fire-and-record a storage_repair push (all configured channels +
/// notification history). Best-effort; failures only log.
async fn notify_storage_repair(message: &str, summary: &str) {
    let title = notification_title();
    if crate::notifications::dispatch_and_record(&title, message, Some("storage_repair"), None, None, Some(summary))
        .await
        .is_none()
    {
        tracing::info!("[storage-boot] notification suppressed (storage_repair type disabled)");
    }
}

async fn finish_boot_notice(
    permit: tokio::sync::MutexGuard<'_, ()>,
    notice: impl std::future::Future<Output = ()>,
) {
    // No repair is running in this branch. Let the owner act on the notice
    // immediately, even if delivery to another provider is still pending.
    drop(permit);
    notice.await;
}

// ───────────────────────── boot-time auto repair ─────────────────────────

/// Settle delay before the boot check. Long enough for fstab mounts and
/// device enumeration to finish; short enough that an unattended fix
/// starts promptly after a corrupting power loss.
const BOOT_CHECK_DELAY: Duration = Duration::from_secs(20);

/// Spawn the boot-time storage check (see `boot_check`). Called once
/// from the server binary's startup path.
pub fn spawn_boot_check(hub: sentryusb_ws::Hub) {
    tokio::spawn(async move { boot_check(hub, AUTO_REPAIR_MARKER).await });
}

/// Boot-time auto repair for /backingfiles, gated by the
/// `storage_auto_repair` preference toggle. Detection signal: the fstab
/// mount already failed by the time we run (XFS refuses corrupt
/// filesystems at boot). A one-shot mount retry filters out the
/// "device enumerated late, nothing actually corrupt" case — if that
/// mount succeeds we deliberately do NOT reboot; the filesystem is fine
/// and slow enumeration is out of scope here.
async fn boot_check(hub: sentryusb_ws::Hub, marker: &str) {
    tokio::time::sleep(BOOT_CHECK_DELAY).await;

    let prefs = crate::preferences::load_prefs();
    let auto_enabled = crate::notification_center::bool_pref(&prefs, "storage_auto_repair", false);
    let force_allowed =
        crate::notification_center::bool_pref(&prefs, "storage_auto_force_repair", false);

    if !auto_enabled {
        tracing::info!("[storage-boot] auto repair skipped: storage_auto_repair disabled");
        return;
    }
    let permit = match REPAIR_LOCK.try_lock() {
        Ok(permit) => permit,
        Err(_) => {
            tracing::info!("[storage-boot] storage repair already active; skipping boot check");
            return;
        }
    };

    // Before anything touches the device, including the mount retry below.
    if xfs_repair_running().await {
        tracing::warn!("[storage-boot] boot check skipped: an xfs_repair is running");
        return;
    }

    let device = resolve_backing_device().await;
    let repairable = match &device {
        Some(d) => repair_eligibility(d).await.is_ok(),
        None => false,
    };
    let mut mounted = resolve_mount_source(&read_proc_mounts().await, BACKINGFILES).is_some();

    // Mount retry: separates "corrupt" from "wasn't there when fstab ran".
    if auto_enabled && repairable && !mounted {
        if let Some(d) = &device {
            let r = run_capture(CMD_TIMEOUT, "mount", &[d.as_str(), BACKINGFILES]).await;
            if r.ok {
                tracing::info!(
                    "[storage-boot] /backingfiles mounted on retry — filesystem is fine, no repair"
                );
                mounted = true;
            }
        }
    }

    match decide_boot_action(auto_enabled, device.is_some(), repairable, mounted, marker_exists(marker)) {
        BootAction::Skip(reason) => {
            tracing::info!("[storage-boot] auto repair skipped: {reason}");
        }
        BootAction::ClearMarker => {
            clear_marker(marker);
            clear_marker(PREFLIGHT_NOTICE_MARKER);
            clear_state_if_settled().await;
        }
        BootAction::NotifyRepeatCorruption => {
            tracing::warn!(
                "[storage-boot] corrupt again after a previous auto repair — not retrying (marker {marker})"
            );
            finish_boot_notice(permit, notify_storage_repair(
                MSG_REPEAT_CORRUPTION,
                "Drive corruption returned. Check power and cables, then run Repair Storage manually.",
            )).await;
        }
        BootAction::Repair => {
            let device = device.expect("BootAction::Repair implies device_found");
            if !database_free() {
                // Only reachable before setup provisioned the drive.
                tracing::info!("[storage-boot] auto repair skipped: the server is using the drive database");
                return;
            }
            if let Err(reason) = repair_eligibility(&device).await {
                tracing::error!("[storage-boot] refusing auto repair of {device}: {reason}");
                return;
            }
            tracing::warn!(
                "[storage-boot] /backingfiles corrupt — starting auto repair (force_allowed={force_allowed})"
            );
            run_repair(hub, device, RepairMode::AutoBoot { force_allowed }, permit).await;
        }
    }
}

// ───────────────────────── runtime resolution ─────────────────────────

fn canonicalize_dev(src: &str) -> String {
    std::fs::canonicalize(src)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| src.to_string())
}

async fn read_proc_mounts() -> String {
    tokio::fs::read_to_string("/proc/mounts").await.unwrap_or_default()
}

/// Parent disk of the root filesystem (`/`), e.g. `mmcblk0`.
async fn root_disk() -> Option<String> {
    let mounts = read_proc_mounts().await;
    let src = resolve_mount_source(&mounts, "/")?;
    Some(parent_disk(&canonicalize_dev(&src)))
}

/// Resolve the device backing `/backingfiles`. Prefers the live mount; falls
/// back to the XFS label when the partition is corrupt and unmounted.
async fn resolve_backing_device() -> Option<String> {
    let mounts = read_proc_mounts().await;
    if let Some(src) = resolve_mount_source(&mounts, BACKINGFILES) {
        return Some(canonicalize_dev(&src));
    }
    // Not mounted (the corruption case): resolve by label from the superblock.
    let r = run_capture(PROBE_TIMEOUT, "blkid", &["-L", XFS_LABEL]).await;
    let dev = r.output.trim();
    if r.ok && dev.starts_with("/dev/") {
        return Some(canonicalize_dev(dev));
    }
    // Fallback: lsblk label scan.
    let r = run_capture(PROBE_TIMEOUT, "lsblk", &["-rno", "PATH,LABEL"]).await;
    for line in r.output.lines() {
        let mut f = line.split_whitespace();
        if let (Some(path), Some(label)) = (f.next(), f.next()) {
            if label == XFS_LABEL && path.starts_with("/dev/") {
                return Some(path.to_string());
            }
        }
    }
    None
}

async fn device_fstype(dev: &str) -> Option<String> {
    let r = run_capture(PROBE_TIMEOUT, "lsblk", &["-rno", "FSTYPE", dev]).await;
    let t = r.output.lines().next().unwrap_or("").trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// `/backingfiles` lives on a different physical disk than root and isn't the
/// onboard SD slot. Reported for the UI; repair itself uses
/// [`repair_eligibility`], which also allows SD-card-only installs.
async fn is_external(dev: &str) -> bool {
    let bp = parent_disk(dev);
    if bp == "mmcblk0" {
        return false;
    }
    match root_disk().await {
        Some(rp) => bp != rp,
        None => true,
    }
}

/// `maj:min` of a block device, from sysfs (no device open).
fn block_dev_id(dev: &str) -> Option<String> {
    let name = std::fs::canonicalize(dev).ok()?;
    let name = name.file_name()?.to_string_lossy().into_owned();
    let id = std::fs::read_to_string(format!("/sys/class/block/{name}/dev")).ok()?;
    Some(id.trim().to_string())
}

/// `maj:min` of the filesystem holding `path` (for swap files).
fn containing_dev_id(path: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(path).ok()?.dev();
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    Some(format!("{major}:{minor}"))
}

/// Whether `dev` may be repaired: a partition (not a whole disk) that is not
/// the root, boot, mutable or swap device. The SD card's own data partition
/// qualifies — xfs_repair only needs the target itself unmounted. Fails
/// closed when the protected devices can't be identified.
async fn repair_eligibility(dev: &str) -> Result<(), String> {
    let dev = dev.to_string();
    tokio::task::spawn_blocking(move || {
        let name = std::fs::canonicalize(&dev)
            .map_err(|e| format!("cannot resolve {dev}: {e}"))?;
        let name = name.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !std::path::Path::new(&format!("/sys/class/block/{name}/partition")).exists() {
            return Err(format!("{dev} is a whole disk, not a data partition"));
        }
        let target = block_dev_id(&dev).ok_or_else(|| format!("cannot identify {dev}"))?;
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| format!("cannot read mounts: {e}"))?;
        let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
        eligibility_from(&target, &sentryusb_drives::storage::visible_mounts(&mountinfo), &swaps)
    })
    .await
    .unwrap_or_else(|e| Err(format!("eligibility check failed: {e}")))
}

/// Pure core of [`repair_eligibility`] once `target` is known to be a partition.
fn eligibility_from(
    target: &str,
    mounts: &std::collections::HashMap<String, String>,
    swaps: &str,
) -> Result<(), String> {
    let root = mounts.get("/").ok_or("cannot identify the system partition")?;
    let mut protected = vec![("system", root.clone())];
    for (label, mp) in [("boot", "/boot/firmware"), ("boot", "/boot"), ("settings (/mutable)", "/mutable")] {
        if let Some(id) = mounts.get(mp) {
            protected.push((label, id.clone()));
        }
    }
    for line in swaps.lines().skip(1) {
        let mut f = line.split_whitespace();
        let (Some(file), Some(kind)) = (f.next(), f.next()) else { continue };
        let id = if kind == "partition" { block_dev_id(file) } else { containing_dev_id(file) };
        match id {
            Some(id) => protected.push(("swap", id)),
            None => return Err(format!("cannot identify swap {file}")),
        }
    }
    if let Some((label, _)) = protected.iter().find(|(_, id)| id == target) {
        return Err(format!("it is the {label} partition"));
    }
    Ok(())
}

/// Whether the device is still mounted anywhere (by device number).
async fn device_still_mounted(dev: &str) -> bool {
    let Some(id) = block_dev_id(dev) else { return true };
    let mountinfo = tokio::fs::read_to_string("/proc/self/mountinfo").await.unwrap_or_default();
    mountinfo.is_empty() || sentryusb_drives::storage::device_is_mounted(&mountinfo, &id)
}

/// True for a genuine XFS *error* line for this device, excluding the benign
/// informational lines XFS prints on every mount/unmount ("Mounting V5
/// Filesystem", "Ending clean mount"). The first cut flagged any line that
/// merely mentioned the device, so a healthy remount after a repair looked
/// like two fresh "errors". An error line must carry a real error keyword.
fn is_xfs_error_line(line: &str, devbase: &str) -> bool {
    let l = line.to_ascii_lowercase();
    if !l.contains("xfs") || !l.contains(devbase) {
        return false;
    }
    // Normal lifecycle chatter — never an error.
    if l.contains("mounting")
        || l.contains("unmounting")
        || l.contains("ending clean mount")
        || l.contains("ending clean unmount")
    {
        return false;
    }
    l.contains("error")
        || l.contains("corrupt")
        || l.contains("shut down")
        || l.contains("shutdown")
        || l.contains("i/o error")
        || l.contains("log recovery")
        || l.contains("inconsistent")
        || l.contains("needs repair")
        || l.contains("metadata corruption")
}

/// Recent genuine XFS error lines for the device, newest last.
async fn recent_xfs_errors(dev: &str) -> Vec<String> {
    let r = run_capture(PROBE_TIMEOUT, "dmesg", &["--ctime"]).await;
    let devbase = dev.strip_prefix("/dev/").unwrap_or(dev).to_ascii_lowercase();
    let mut out: Vec<String> = r
        .output
        .lines()
        .filter(|l| is_xfs_error_line(l, &devbase))
        .map(|s| s.trim().to_string())
        .collect();
    let len = out.len();
    if len > 12 {
        out = out.split_off(len - 12);
    }
    out
}

fn cam_disk_present() -> bool {
    std::path::Path::new(&format!("{BACKINGFILES}/cam_disk.bin")).exists()
}

// ───────────────────────── persisted transcript ─────────────────────────

fn persist_log(buf: &str) -> Option<String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!("storage_repair_{ts}.log");
    let path = format!("{REPAIR_LOG_DIR}/{name}");
    if !sentryusb_drives::storage::is_separate_mount(REPAIR_LOG_DIR) {
        tracing::warn!("repair log not saved: {REPAIR_LOG_DIR} is not mounted");
        return None;
    }
    match std::fs::write(&path, buf) {
        Ok(_) => Some(name),
        Err(e) => {
            tracing::warn!("failed to write repair log {path}: {e}");
            None
        }
    }
}

fn latest_repair_log() -> Option<String> {
    let rd = std::fs::read_dir(REPAIR_LOG_DIR).ok()?;
    let mut best: Option<(u64, String)> = None;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(ts) = n
            .strip_prefix("storage_repair_")
            .and_then(|x| x.strip_suffix(".log"))
            .and_then(|x| x.parse::<u64>().ok())
        {
            if best.as_ref().is_none_or(|(b, _)| ts > *b) {
                best = Some((ts, n));
            }
        }
    }
    best.map(|(_, n)| n)
}

// ───────────────────────── GET /api/storage/health ─────────────────────────

#[derive(Serialize)]
struct StorageHealth {
    /// `healthy` | `unmounted` | `corrupt` | `missing_images` | `not_repairable`
    state: String,
    /// Whether `/backingfiles` is on a separate external drive.
    external: bool,
    /// Whether Repair Storage can run on this device (gates the UI).
    repairable: bool,
    /// Why not, when `repairable` is false.
    repair_blocked: Option<String>,
    device: Option<String>,
    fstype: Option<String>,
    mounted: bool,
    mountpoint: String,
    cam_disk_present: bool,
    /// Recent XFS kernel errors mentioning the device, newest last.
    dmesg_errors: Vec<String>,
    /// Filename of the most recent persisted repair transcript, if any.
    last_repair_log: Option<String>,
}

/// GET /api/storage/health
pub async fn storage_health() -> (StatusCode, Json<serde_json::Value>) {
    let device = resolve_backing_device().await;
    let mounts = read_proc_mounts().await;
    let mounted = resolve_mount_source(&mounts, BACKINGFILES).is_some();
    let external = match &device {
        Some(d) => is_external(d).await,
        None => false,
    };
    let eligibility = match &device {
        Some(d) => repair_eligibility(d).await,
        None => Err("no footage partition found".to_string()),
    };
    let repairable = eligibility.is_ok();
    let fstype = match &device {
        Some(d) => device_fstype(d).await,
        None => None,
    };
    let cam = cam_disk_present();
    let dmesg_errors = if repairable {
        match &device {
            Some(d) => recent_xfs_errors(d).await,
            None => vec![],
        }
    } else {
        vec![]
    };

    let state = if !repairable {
        "not_repairable"
    } else if mounted {
        if cam { "healthy" } else { "missing_images" }
    } else if !dmesg_errors.is_empty() {
        "corrupt"
    } else {
        "unmounted"
    };

    let health = StorageHealth {
        state: state.to_string(),
        external,
        repairable,
        repair_blocked: eligibility.err(),
        device,
        fstype,
        mounted,
        mountpoint: BACKINGFILES.to_string(),
        cam_disk_present: cam,
        dmesg_errors,
        last_repair_log: latest_repair_log(),
    };
    (
        StatusCode::OK,
        Json(serde_json::to_value(health).unwrap_or_else(|_| serde_json::json!({}))),
    )
}

// ───────────────────────── POST /api/storage/repair ─────────────────────────

#[derive(Deserialize, Default)]
struct RepairRequest {
    /// Authorizes the destructive `xfs_repair -L` last resort. Without it the
    /// flow hard-stops at the escalation gate and broadcasts `needs_force`.
    #[serde(default)]
    confirm_destructive: bool,
}

/// Broadcasts each repair step over the `storage_repair` WS channel and
/// accumulates the full transcript for persistence.
struct RepairLog {
    hub: sentryusb_ws::Hub,
    buf: String,
}

impl RepairLog {
    fn line(&mut self, phase: &str, line: impl Into<String>) {
        let line = line.into();
        self.buf.push_str(&line);
        self.buf.push('\n');
        self.hub.broadcast(
            "storage_repair",
            serde_json::json!({ "status": "running", "phase": phase, "line": line }),
        );
    }

    fn cmd(&mut self, phase: &str, label: &str, r: &CmdResult) {
        self.line(phase, format!("$ {label}"));
        for l in r.output.lines() {
            self.line(phase, format!("  {l}"));
        }
        self.line(phase, format!("  → exit {}", r.code));
    }
}

/// POST /api/storage/repair
///
/// Validates synchronously (so the caller gets an immediate 4xx on a bad
/// precondition) then spawns the repair, streaming progress over WS. Returns
/// `{ "status": "started" }` on a valid request.
pub async fn storage_repair(State(hub): State<sentryusb_ws::Hub>, body: String) -> (StatusCode, Json<serde_json::Value>) {
    let permit = match REPAIR_LOCK.try_lock() {
        Ok(permit) => permit,
        Err(_) => return crate::json_error(
            StatusCode::CONFLICT, "Storage repair is already in progress. Wait for it to finish.",
        ),
    };
    let req: RepairRequest = if body.trim().is_empty() {
        RepairRequest::default()
    } else {
        match serde_json::from_str(&body) {
            Ok(req) => req,
            Err(e) => return crate::json_error(StatusCode::BAD_REQUEST, &format!("Invalid repair request: {e}")),
        }
    };
    if !database_free() {
        return crate::json_error(
            StatusCode::CONFLICT,
            "SentryUSB is using the footage drive right now, so it can't be repaired. Reboot the Pi; if the drive still can't be mounted, Repair Storage will be available.",
        );
    }
    if xfs_repair_running().await {
        return crate::json_error(
            StatusCode::CONFLICT,
            "An earlier xfs_repair is still running. Wait for it to finish or reboot the Pi.",
        );
    }

    let device = match resolve_backing_device().await {
        Some(d) => d,
        None => {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                "Could not find the camera storage device (no /backingfiles mount and no 'backingfiles'-labelled partition).",
            )
        }
    };
    if let Err(reason) = repair_eligibility(&device).await {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            &format!("Refusing to repair {device}: {reason}."),
        );
    }
    if let Some(fs) = device_fstype(&device).await {
        if fs != "xfs" {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                &format!("Storage repair currently supports XFS only (found '{fs}')."),
            );
        }
    }

    tokio::spawn(async move {
        run_repair(
            hub,
            device,
            RepairMode::Interactive { confirm_destructive: req.confirm_destructive },
            permit,
        )
        .await;
    });

    (StatusCode::OK, Json(serde_json::json!({ "status": "started" })))
}

// ───────────────────────── saved repair state ─────────────────────────

/// Saved outcome of the latest repair, so the card can show it after a
/// page refresh or a service restart. Lives on /mutable (its own partition,
/// also where the transcripts go); RAM-only when /mutable isn't mounted.
const STATE_FILE: &str = "/mutable/storage_repair_state.json";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct RepairRecord {
    /// `running` | `needs_force` | `rebooting` | `failed` | `interrupted`
    phase: String,
    device: Option<String>,
    message: Option<String>,
    log_file: Option<String>,
    started: u64,
    updated: u64,
    /// Boot the record was written in (tells "rebooted" from "service restarted").
    #[serde(default)]
    boot_id: Option<String>,
}

fn current_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok().map(|s| s.trim().to_string())
}

static RECORD: std::sync::Mutex<Option<RepairRecord>> = std::sync::Mutex::new(None);
/// Bumped per save; a write abandoned by its timeout must not publish an
/// older record over a newer one once a stalled /mutable wakes up.
static RECORD_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// One state-file write at a time (they share the temp file name).
static RECORD_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// False when the last save didn't reach /mutable.
static RECORD_SAVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn current_record() -> Option<RepairRecord> {
    RECORD.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Update the record in RAM and try to save it. A hung /mutable must not
/// hang the repair: the save gets 10 seconds.
async fn save_record(record: Option<RepairRecord>) -> bool {
    *RECORD.lock().unwrap_or_else(|e| e.into_inner()) = record.clone();
    let generation = RECORD_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let write = tokio::task::spawn_blocking(move || {
        let _one_at_a_time = RECORD_WRITE.lock().unwrap_or_else(|e| e.into_inner());
        if RECORD_GEN.load(std::sync::atomic::Ordering::SeqCst) != generation {
            return Ok(()); // a newer record is queued behind us
        }
        match &record {
            Some(r) => write_durable(STATE_FILE, &serde_json::to_vec_pretty(r)?),
            None => {
                if sentryusb_drives::storage::is_separate_mount(REPAIR_LOG_DIR) {
                    match std::fs::remove_file(STATE_FILE) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                        _ => {}
                    }
                }
                Ok(())
            }
        }
    });
    let saved = matches!(tokio::time::timeout(Duration::from_secs(10), write).await, Ok(Ok(Ok(()))));
    if !saved {
        tracing::warn!("[storage-repair] could not save the repair state to {STATE_FILE}");
    }
    RECORD_SAVED.store(saved, std::sync::atomic::Ordering::SeqCst);
    saved
}

async fn set_phase(phase: &str, device: &str, message: Option<String>, log_file: Option<String>) -> bool {
    let now = unix_seconds();
    let started = current_record()
        .filter(|r| r.phase == "running")
        .map(|r| r.started)
        .unwrap_or(now);
    save_record(Some(RepairRecord {
        phase: phase.into(),
        device: Some(device.into()),
        message,
        log_file,
        started,
        updated: now,
        boot_id: current_boot_id(),
    }))
    .await
}

/// What a saved record means after this process starts. A repair runs
/// inside this process, so one saved as running has stopped. A reboot
/// after a repair is finished business once the Pi really rebooted; if
/// only the service restarted (same boot), the reboot never happened and
/// the drive is still unmounted.
fn after_restart(record: RepairRecord, boot_id: Option<&str>) -> Option<RepairRecord> {
    match record.phase.as_str() {
        "running" => Some(RepairRecord {
            phase: "interrupted".into(),
            message: Some("The repair stopped before it finished (the Pi restarted or lost power). Run Repair Storage again.".into()),
            updated: unix_seconds(),
            ..record
        }),
        "rebooting" if boot_id.is_some() && record.boot_id.as_deref() == boot_id => Some(RepairRecord {
            phase: "interrupted".into(),
            message: Some("The repair finished, but the Pi didn't reboot afterwards. Reboot the Pi to reconnect the drive to the car.".into()),
            updated: unix_seconds(),
            ..record
        }),
        "rebooting" => None,
        _ => Some(record),
    }
}

/// Called once at startup, before any repair can start.
pub async fn recover_state_after_restart() {
    // Bounded: a stalled /mutable must not keep the recovery UI from starting.
    let read = tokio::task::spawn_blocking(|| {
        if !sentryusb_drives::storage::is_separate_mount(REPAIR_LOG_DIR) {
            return None;
        }
        let text = std::fs::read_to_string(STATE_FILE).ok()?;
        serde_json::from_str::<RepairRecord>(&text).ok()
    });
    let loaded = match tokio::time::timeout(Duration::from_secs(5), read).await {
        Ok(Ok(record)) => record,
        _ => {
            tracing::warn!("[storage-repair] could not read {STATE_FILE} in time; starting without it");
            None
        }
    };
    let Some(record) = loaded else { return };
    let next = after_restart(record.clone(), current_boot_id().as_deref());
    if next.as_ref() != Some(&record) {
        save_record(next).await;
    } else {
        *RECORD.lock().unwrap_or_else(|e| e.into_inner()) = next;
    }
    clear_state_if_settled().await;
}

/// Storage is working again: an old failure no longer applies.
async fn clear_state_if_settled() {
    let settled = current_record().is_some_and(|r| r.phase != "running")
        && resolve_mount_source(&read_proc_mounts().await, BACKINGFILES).is_some()
        && tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(|| {
                sentryusb_drives::storage::check_footage().is_ok()
                    && sentryusb_gadget::validate_storage().is_ok()
            }),
        )
        .await
        .is_ok_and(|r| r.unwrap_or(false));
    if settled {
        save_record(None).await;
    }
}

/// GET /api/storage/repair — the latest repair outcome, for the card to
/// restore after a refresh.
pub async fn repair_status() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "active": REPAIR_LOCK.try_lock().is_err(),
            "record": current_record(),
            "saved": RECORD_SAVED.load(std::sync::atomic::Ordering::SeqCst),
        })),
    )
}

// ───────────────────────── the repair ─────────────────────────

/// Bring back what quiescing stopped, after a repair that won't reboot.
/// archiveloop waits for usable storage on its own.
async fn resume_services(log: &mut RepairLog) {
    for unit in [TELEMETRY_UNIT, ARCHIVE_UNIT] {
        let r = run_capture(CMD_TIMEOUT, "systemctl", &["start", unit]).await;
        if !r.ok {
            log.cmd("resume", &format!("systemctl start {unit}"), &r);
        }
    }
}

/// End a repair that didn't succeed: save the outcome, tell the UI, restart
/// what was stopped (if anything was), and notify in auto mode.
async fn fail_repair(
    hub: &sentryusb_ws::Hub,
    log: &mut RepairLog,
    device: &str,
    quiesced: bool,
    error: &str,
    push: Option<(&str, &str)>,
) {
    if quiesced {
        resume_services(log).await;
    }
    let log_file = persist_log(&log.buf);
    set_phase("failed", device, Some(error.to_string()), log_file.clone()).await;
    hub.broadcast(
        "storage_repair",
        serde_json::json!({ "status": "error", "device": device, "log_file": log_file, "error": error }),
    );
    if let Some((message, summary)) = push {
        notify_storage_repair(message, summary).await;
    }
}

/// Unmount the footage drive and everything exposed from it, then prove
/// the device is mounted nowhere.
async fn release_mounts(log: &mut RepairLog, device: &str) -> bool {
    let mut mps: Vec<&str> = SUBMOUNTS.to_vec();
    mps.push(BACKINGFILES);
    for mp in mps {
        let r = run_capture(CMD_TIMEOUT, "umount", &[mp]).await;
        if !r.ok && !r.output.contains("not mounted") && !r.output.contains("not found") {
            log.cmd("unmount", &format!("umount {mp}"), &r);
        }
    }
    let r = run_capture(CMD_TIMEOUT, "umount", &[device]).await;
    if !r.ok && !r.output.contains("not mounted") {
        log.cmd("unmount", &format!("umount {device}"), &r);
    }
    if device_still_mounted(device).await {
        log.line("unmount", format!("{device} is still mounted; not repairing a filesystem in use."));
        return false;
    }
    true
}

async fn run_repair(
    hub: sentryusb_ws::Hub, device: String, mode: RepairMode,
    _permit: tokio::sync::MutexGuard<'static, ()>,
) {
    let mut log = RepairLog { hub: hub.clone(), buf: String::new() };
    let t = CMD_TIMEOUT;
    let (auto, force_allowed) = match mode {
        RepairMode::Interactive { confirm_destructive } => (false, confirm_destructive),
        RepairMode::AutoBoot { force_allowed } => (true, force_allowed),
    };
    let hard_fail_push = auto.then_some((MSG_HARD_FAIL, "Drive repair failed. Check the drive's power, cable and enclosure, then open Repair Storage."));

    log.line(
        "preflight",
        format!("Repairing {device} (XFS backingfiles); auto={auto} force_allowed={force_allowed}"),
    );

    // Kernel ABI matters, not the Rust server's build target or userspace
    // bitness: Pi OS armhf can run on an aarch64 kernel. Never silently fall
    // back to armhf if preparation of the required arm64 tool fails.
    let tool = match select_repair_tool(REPAIR_TOOL_SELECTOR).await {
        Ok(tool) => tool,
        Err(error) => {
            log.line("preflight", &error);
            let log_file = persist_log(&log.buf);
            let message = format!("Cannot prepare compatible XFS repair tool: {error}. Storage was not changed.");
            set_phase("failed", &device, Some(message.clone()), log_file.clone()).await;
            hub.broadcast("storage_repair", serde_json::json!({
                "status": "error", "device": device, "log_file": log_file, "error": message,
            }));
            if auto && claim_preflight_notice(PREFLIGHT_NOTICE_MARKER, unix_seconds()) {
                notify_storage_repair("Automatic storage repair could not start", &error).await;
            }
            return;
        }
    };
    log.line("preflight", format!("Selected XFS repair command: {}", tool.0.join(" ")));
    let version = tool.run(PROBE_TIMEOUT, &["-V"]).await;
    log.cmd("preflight", "selected xfs_repair -V", &version);
    if !version.ok {
        let log_file = persist_log(&log.buf);
        let message = "Selected XFS repair tool cannot run. Storage was not changed.".to_string();
        set_phase("failed", &device, Some(message.clone()), log_file.clone()).await;
        hub.broadcast("storage_repair", serde_json::json!({
            "status": "error", "device": device, "log_file": log_file, "error": message,
        }));
        if auto && claim_preflight_notice(PREFLIGHT_NOTICE_MARKER, unix_seconds()) {
            notify_storage_repair("Automatic storage repair could not start", &version.output).await;
        }
        return;
    }
    // Tool acquisition is not a repair attempt. An offline first boot must
    // not suppress a later attempt once the compatible tool is available.
    // An automatic attempt that can't be recorded doesn't start: without the
    // marker, a repair that ends in a reboot could repeat on every boot.
    if auto && !write_marker(AUTO_REPAIR_MARKER) {
        let message = "Automatic repair did not start: its attempt couldn't be recorded on /mutable. Run Repair Storage from Settings.".to_string();
        log.line("preflight", &message);
        set_phase("failed", &device, Some(message.clone()), None).await;
        hub.broadcast("storage_repair", serde_json::json!({ "status": "error", "device": device, "error": message }));
        return;
    }
    clear_marker(PREFLIGHT_NOTICE_MARKER);
    set_phase("running", &device, None, None).await;

    // ── 1. Quiesce — NEVER stop the `sentryusb` service (it's us). ──
    // The server runs without a database here (admission requires it), so
    // its own process holds nothing on the drive.
    log.line("quiesce", "Stopping telemetry, the archive loop and the USB drive (the web UI stays up)…");
    for unit in [TELEMETRY_UNIT, ARCHIVE_UNIT] {
        let r = run_capture(t, "systemctl", &["stop", unit]).await;
        if !r.ok {
            log.cmd("quiesce", &format!("systemctl stop {unit}"), &r);
        }
    }
    let _ = run_capture(t, "bash", &["-c", "killall archiveloop 2>/dev/null || true"]).await;
    if run_capture(PROBE_TIMEOUT, "pgrep", &["-f", "/root/bin/archiveloop"]).await.ok {
        log.line("quiesce", "The archive loop is still running.");
        fail_repair(&hub, &mut log, &device, true,
            "Couldn't stop the archive loop, so the drive wasn't touched. Reboot the Pi and try again.", None).await;
        return;
    }
    let gadget = tokio::task::spawn_blocking(|| {
        sentryusb_gadget::disable()?;
        if sentryusb_gadget::is_active() {
            anyhow::bail!("the USB drive is still presented to the car");
        }
        Ok(())
    })
    .await;
    match gadget {
        Ok(Ok(())) => log.line("quiesce", "USB drive disconnected from the car."),
        Ok(Err(e)) => {
            log.line("quiesce", format!("USB disconnect failed: {e:#}"));
            fail_repair(&hub, &mut log, &device, true,
                "Couldn't disconnect the USB drive from the car, so the drive wasn't touched. Reboot the Pi and try again.", None).await;
            return;
        }
        Err(e) => {
            log.line("quiesce", format!("USB disconnect task error: {e}"));
            fail_repair(&hub, &mut log, &device, true,
                "Couldn't disconnect the USB drive from the car, so the drive wasn't touched. Reboot the Pi and try again.", None).await;
            return;
        }
    }

    // ── 2. Release mounts; xfs_repair must never run on a mounted filesystem. ──
    log.line("unmount", "Releasing mounts…");
    if !release_mounts(&mut log, &device).await {
        fail_repair(&hub, &mut log, &device, true,
            "The footage drive is still in use, so it wasn't repaired. Reboot the Pi and try again.", None).await;
        return;
    }

    // ── 3. Read-only diagnosis. ──
    log.line("dryrun", "Running read-only check (xfs_repair -n)…");
    let dry = tool.run(t, &["-n", &device]).await;
    log.cmd("dryrun", &format!("xfs_repair -n {device}"), &dry);

    // ── 4. Repair without clearing the log, with a mount-to-replay-log retry. ──
    log.line("repair", "Attempting repair without clearing the log (xfs_repair)…");
    let mut rep = tool.run(t, &[&device]).await;
    log.cmd("repair", &format!("xfs_repair {device}"), &rep);
    let mut replay_device_trouble = false;

    if !rep.ok && needs_log_replay(&rep.output) {
        log.line("repair", "Dirty log detected — mounting to replay it, then retrying…");
        let m = run_capture(t, "mount", &[&device, BACKINGFILES]).await;
        log.cmd("repair", &format!("mount {device} {BACKINGFILES}"), &m);
        // A replay mount that timed out, couldn't start or hit device errors
        // gave no verdict on the log: automatic mode won't clear it.
        replay_device_trouble = !m.ok && (m.code <= 0 || device_trouble(&m.output));
        if m.ok {
            let u = run_capture(t, "umount", &[BACKINGFILES]).await;
            log.cmd("repair", &format!("umount {BACKINGFILES}"), &u);
            if device_still_mounted(&device).await {
                fail_repair(&hub, &mut log, &device, true,
                    "The footage drive replayed its log but couldn't be unmounted again. Reboot the Pi; the drive may now mount normally.", None).await;
                return;
            }
            rep = tool.run(t, &[&device]).await;
            log.cmd("repair", &format!("xfs_repair {device} (after log replay)"), &rep);
        } else {
            log.line("repair", "Mount failed — the log cannot be replayed this way.");
        }
    }

    // ── 5. Escalation gate: policy differs by mode (see escalation_action). ──
    // Automatic mode only clears the log after a real repair verdict that
    // clearing it could change — never after a timeout or I/O errors.
    let mut force_ran = false;
    match decide_escalation(&rep, auto, force_allowed, replay_device_trouble) {
        Escalation::Proceed | Escalation::Fail => {}
        Escalation::StopNeedsForce => {
            let log_file = persist_log(&log.buf);
            // Saved before the UI hears about it, so a refresh still shows it.
            set_phase("needs_force", &device, Some(MSG_LOG_RESET.into()), log_file.clone()).await;
            resume_services(&mut log).await;
            hub.broadcast(
                "storage_repair",
                serde_json::json!({
                    "status": "needs_force",
                    "device": device,
                    "log_file": log_file,
                    "message": MSG_LOG_RESET,
                }),
            );
            if auto {
                notify_storage_repair(MSG_NEEDS_MANUAL_FORCE, "Automatic drive repair failed. Open Settings → System → Repair Storage for manual recovery.").await;
            }
            return;
        }
        Escalation::RunForce => {
            log.line(
                "repair",
                if auto {
                    "Auto force fix enabled — clearing the XFS log (xfs_repair -L)…"
                } else {
                    "Confirmed — clearing the XFS log (xfs_repair -L)…"
                },
            );
            if device_still_mounted(&device).await {
                fail_repair(&hub, &mut log, &device, true,
                    "The footage drive became mounted again, so the log was not cleared. Reboot the Pi and try again.", hard_fail_push).await;
                return;
            }
            rep = tool.run(t, &["-L", &device]).await;
            log.cmd("repair", &format!("xfs_repair -L {device}"), &rep);
            force_ran = true;
        }
    }

    if !rep.ok {
        fail_repair(&hub, &mut log, &device, true,
            "xfs_repair could not repair the filesystem. Review the log for filesystem, repair-tool, power, or connection errors; this alone does not establish SSD hardware failure.",
            hard_fail_push).await;
        return;
    }

    // ── 6. Verify the real filesystem and the car's disk images. ──
    log.line("verify", "Repair succeeded — verifying the drive and its disk images…");
    let m = run_capture(t, "mount", &[&device, BACKINGFILES]).await;
    if !m.ok {
        log.cmd("verify", &format!("mount {device} {BACKINGFILES}"), &m);
        fail_repair(&hub, &mut log, &device, true,
            "xfs_repair finished, but the footage drive still won't mount. Review the log, then reboot the Pi and run Repair Storage again.",
            hard_fail_push).await;
        return;
    }
    let lost_found = std::fs::read_dir(format!("{BACKINGFILES}/lost+found"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    let images = tokio::task::spawn_blocking(|| {
        sentryusb_drives::storage::check_footage()?;
        sentryusb_gadget::validate_storage()
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    let cam_present = cam_disk_present();
    log.line("verify", format!("cam_disk.bin present: {cam_present}; lost+found entries: {lost_found}"));
    let u = run_capture(t, "umount", &[BACKINGFILES]).await;
    if !u.ok {
        log.cmd("verify", &format!("umount {BACKINGFILES}"), &u);
    }
    if let Err(e) = images {
        log.line("verify", format!("Disk images are not usable: {e:#}"));
        fail_repair(&hub, &mut log, &device, true,
            "The filesystem was repaired, but the car's disk images are missing or unusable. Re-run the Setup Wizard to recreate them (your other files stay).",
            auto.then_some((MSG_HARD_FAIL, "Drive repaired, but the car's disk images are missing. Open Setup to recreate them."))).await;
        return;
    }

    // ── 7. Reboot: the normal boot path brings storage, the database,
    //      archiving and the USB drive back in order. ──
    let log_file = persist_log(&log.buf);
    let secs = REBOOT_COUNTDOWN.as_secs();
    let message = format!(
        "Repair complete and the disk images check out{}. Rebooting in {secs} seconds to reconnect the drive to the car.",
        if lost_found > 0 { format!(" ({lost_found} recovered item(s) are in lost+found)") } else { String::new() }
    );
    set_phase("rebooting", &device, Some(message.clone()), log_file.clone()).await;
    hub.broadcast(
        "storage_repair",
        serde_json::json!({
            "status": "rebooting",
            "device": device,
            "cam_disk_present": cam_present,
            "lost_found_count": lost_found,
            "log_file": log_file,
            "seconds": secs,
            "message": message,
        }),
    );
    if auto {
        let push = if force_ran { MSG_FORCE_SUCCESS } else { MSG_AUTO_SUCCESS };
        notify_storage_repair(push, "Drive filesystem repaired. Rebooting the Pi now.").await;
    }
    // The repair slot stays held through the countdown, so the reopen
    // probe and USB enable wait for the reboot.
    tokio::time::sleep(REBOOT_COUNTDOWN).await;
    tracing::info!("[storage-repair] repair complete — rebooting");
    let r = run_capture(t, "reboot", &[]).await;
    if !r.ok {
        log.cmd("reboot", "reboot", &r);
        fail_repair(&hub, &mut log, &device, true,
            "The repair succeeded, but the Pi couldn't reboot itself. Reboot it from Settings to reconnect the drive.", None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repeat_corruption_notice_does_not_hold_repair_admission() {
        let gate = tokio::sync::Mutex::new(());
        let permit = gate.lock().await;
        let (started, receiving) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let notice = async {
            started.send(()).unwrap();
            finished.await.unwrap();
        };
        let manual_repair = async {
            receiving.await.unwrap();
            assert!(gate.try_lock().is_ok(), "manual repair blocked while notification is in flight");
            finish.send(()).unwrap();
        };
        tokio::join!(finish_boot_notice(permit, notice), manual_repair);
    }

    #[test]
    fn preflight_notice_is_rate_limited_without_consuming_repair_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let notice = dir.path().join("notice");
        let notice = notice.to_str().unwrap();
        let attempt = dir.path().join("attempt");
        assert!(claim_preflight_notice(notice, 100_000));
        assert!(!claim_preflight_notice(notice, 100_000), "same-boot alert repeated");
        assert!(!claim_preflight_notice(notice, 186_399), "alert repeated within 24 hours");
        assert!(claim_preflight_notice(notice, 186_400));
        assert!(!attempt.exists());
        assert_eq!(decide_boot_action(true, true, true, false, attempt.exists()), BootAction::Repair);
        clear_marker(notice);
        assert!(claim_preflight_notice(notice, 186_401), "new incident should notify immediately");
    }

    #[test]
    fn preflight_notice_recovers_from_bad_timestamp_or_clock_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let notice = dir.path().join("notice");
        let notice = notice.to_str().unwrap();
        std::fs::write(notice, "corrupt").unwrap();
        assert!(claim_preflight_notice(notice, 100_000));
        assert!(!claim_preflight_notice(notice, 100_001));
        assert!(claim_preflight_notice(notice, 50_000), "future timestamp must not suppress alerts indefinitely");
        assert!(!claim_preflight_notice(notice, 50_001));
    }

    #[tokio::test]
    async fn overlapping_repair_request_is_rejected_before_device_probe() {
        // Represents a boot/interactive repair already in preparation or I/O.
        // Reject a new request immediately, even if device discovery would fail.
        let _permit = REPAIR_LOCK.lock().await;
        let request = storage_repair(State(sentryusb_ws::Hub::new()), "{}".into());
        let (status, Json(body)) = tokio::time::timeout(Duration::from_secs(2), request)
            .await.expect("busy repair request must not wait for device probes");
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("already"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_warning_does_not_reject_valid_stdout() {
        let result = select_repair_tool(
            "import sys; print('extractor warning', file=sys.stderr); print('[\"/usr/sbin/xfs_repair\"]')",
        ).await;
        let tool = result.unwrap_or_else(|error| panic!("successful selection rejected: {error}"));
        assert_eq!(tool.0, vec!["/usr/sbin/xfs_repair"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_never_accepts_stderr_as_the_command() {
        let result = select_repair_tool(
            "import sys; print('[\"/usr/sbin/xfs_repair\"]', file=sys.stderr)",
        ).await;
        assert!(result.is_err(), "stderr must never supply the executable selection");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_failure_preserves_diagnostics_and_rejects_json() {
        let result = select_repair_tool(
            "import sys; print('[\"/usr/sbin/xfs_repair\"]'); print('download failed', file=sys.stderr); sys.exit(1)",
        ).await;
        assert!(matches!(result, Err(error) if error.contains("download failed")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_rejects_empty_relative_or_polluted_stdout() {
        for output in ["[]", "[\"relative/path\"]", "warning\\n[\"/usr/sbin/xfs_repair\"]"] {
            let script = format!("print({})", serde_json::to_string(output).unwrap());
            assert!(select_repair_tool(&script).await.is_err(), "accepted {output}");
        }
    }

    #[test]
    fn selected_tool_is_used_for_check_normal_retry_and_force() {
        let tool = RepairTool(vec!["/tools/loader".into(), "--library-path".into(),
            "/tools/lib".into(), "/tools/xfs_repair".into()]);
        for args in [vec!["-n", "/dev/sda2"], vec!["/dev/sda2"],
                     vec!["/dev/sda2"], vec!["-L", "/dev/sda2"]] {
            let (program, forwarded) = tool.invocation(&args);
            assert_eq!(program, "/tools/loader");
            assert_eq!(&forwarded[..3], &["--library-path", "/tools/lib", "/tools/xfs_repair"]);
            assert_eq!(&forwarded[3..], &args);
        }
        let native = RepairTool(vec!["/usr/sbin/xfs_repair".into()]);
        assert_eq!(native.invocation(&["-L", "/dev/sda2"]),
            ("/usr/sbin/xfs_repair".into(), vec!["-L".into(), "/dev/sda2".into()]));
    }

    #[test]
    fn parent_disk_strips_partition_suffix() {
        assert_eq!(parent_disk("/dev/sda2"), "sda");
        assert_eq!(parent_disk("sda2"), "sda");
        assert_eq!(parent_disk("/dev/sda"), "sda");
        assert_eq!(parent_disk("mmcblk0p2"), "mmcblk0");
        assert_eq!(parent_disk("/dev/mmcblk0p2"), "mmcblk0");
        assert_eq!(parent_disk("mmcblk0"), "mmcblk0");
        assert_eq!(parent_disk("nvme0n1p3"), "nvme0n1");
        assert_eq!(parent_disk("nvme0n1"), "nvme0n1");
    }

    #[test]
    fn resolve_mount_source_finds_backingfiles() {
        let mounts = "\
sysfs /sys sysfs rw 0 0
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
/dev/sda1 /mutable ext4 rw 0 0
/dev/sda2 /backingfiles xfs rw,noatime 0 0
tmpfs /run tmpfs rw 0 0";
        assert_eq!(resolve_mount_source(mounts, "/backingfiles").as_deref(), Some("/dev/sda2"));
        assert_eq!(resolve_mount_source(mounts, "/").as_deref(), Some("/dev/mmcblk0p2"));
        assert_eq!(resolve_mount_source(mounts, "/mutable").as_deref(), Some("/dev/sda1"));
        assert_eq!(resolve_mount_source(mounts, "/nope"), None);
    }

    #[test]
    fn resolve_mount_source_ignores_short_lines() {
        // A malformed/short line must not panic or mis-resolve.
        assert_eq!(resolve_mount_source("garbage\n/dev/sda2 /backingfiles xfs rw 0 0", "/backingfiles").as_deref(), Some("/dev/sda2"));
    }

    #[test]
    fn xfs_error_filter_ignores_benign_mount_lines() {
        let dev = "sda2";
        // Benign lifecycle chatter — the lines a healthy remount prints.
        // These were wrongly flagged as "2 recent filesystem errors".
        assert!(!is_xfs_error_line(
            "[Sun Jun 14 05:34:14 2026] XFS (sda2): Mounting V5 Filesystem b1a5fe90",
            dev
        ));
        assert!(!is_xfs_error_line(
            "[Sun Jun 14 05:34:14 2026] XFS (sda2): Ending clean mount",
            dev
        ));
        // Real errors from the actual incident MUST still be flagged.
        assert!(is_xfs_error_line("XFS (sda2): Metadata CRC error detected", dev));
        assert!(is_xfs_error_line(
            "XFS (sda2): Filesystem has been shut down due to log error (0x2).",
            dev
        ));
        assert!(is_xfs_error_line("XFS (sda2): log mount/recovery failed: error -74", dev));
        // A different volume's noise is ignored.
        assert!(!is_xfs_error_line("XFS (sdb1): Metadata CRC error detected", dev));
        // Non-XFS lines are ignored.
        assert!(!is_xfs_error_line("EXT4-fs (sda1): error count", dev));
    }

    #[test]
    fn decide_boot_action_covers_all_branches() {
        use BootAction::*;
        // Toggle off → never touch anything.
        assert_eq!(decide_boot_action(false, true, true, false, false), Skip("storage_auto_repair disabled"));
        // No device (SSD unplugged / setup incomplete) → skip.
        assert_eq!(decide_boot_action(true, false, false, false, false), Skip("no backingfiles device found"));
        // Not a repairable data partition → skip (same gate as the manual card).
        assert_eq!(decide_boot_action(true, true, false, false, false), Skip("backingfiles is not a repairable data partition"));
        // Healthy mount → clear any stale incident marker.
        assert_eq!(decide_boot_action(true, true, true, true, false), ClearMarker);
        assert_eq!(decide_boot_action(true, true, true, true, true), ClearMarker);
        // Corrupt again after a previous auto attempt → notify, don't loop.
        assert_eq!(decide_boot_action(true, true, true, false, true), NotifyRepeatCorruption);
        // Corrupt, first time → repair.
        assert_eq!(decide_boot_action(true, true, true, false, false), Repair);
    }

    #[test]
    fn escalation_auto_forces_on_any_failure_interactive_only_on_dirty_log() {
        use Escalation::*;
        // Success → proceed regardless of mode.
        assert_eq!(escalation_action(true, false, true, true), Proceed);
        assert_eq!(escalation_action(true, true, false, false), Proceed);
        // AUTO: -L on ANY failure when allowed (user's explicit choice),
        // even when the output is not a dirty-log failure.
        assert_eq!(escalation_action(false, false, true, true), RunForce);
        assert_eq!(escalation_action(false, true, true, true), RunForce);
        // AUTO without the force toggle → stop and tell the user.
        assert_eq!(escalation_action(false, false, true, false), StopNeedsForce);
        assert_eq!(escalation_action(false, true, true, false), StopNeedsForce);
        // INTERACTIVE: unchanged semantics — -L only for dirty-log,
        // and only when the user confirmed.
        assert_eq!(escalation_action(false, true, false, true), RunForce);
        assert_eq!(escalation_action(false, true, false, false), StopNeedsForce);
        assert_eq!(escalation_action(false, false, false, true), Fail);
        assert_eq!(escalation_action(false, false, false, false), Fail);
    }

    fn result(ok: bool, code: i32, output: &str) -> CmdResult {
        CmdResult { ok, code, output: output.into(), stdout: String::new(), stderr: String::new() }
    }

    #[test]
    fn auto_mode_only_clears_the_log_after_a_real_verdict() {
        // A finished repair that reported damage: clearing the log may help.
        assert!(force_may_help(&result(false, 1, "Phase 3 ... fatal error -- couldn't map inode")));
        // Timeout / spawn failure / signal: no verdict.
        assert!(!force_may_help(&result(false, -1, "(timed out after 300s)")));
        // Device I/O trouble: clearing the log can't fix the disk.
        assert!(!force_may_help(&result(false, 1, "pread: Input/output error")));
        assert!(!force_may_help(&result(false, 1, "can't read superblock: No such device")));
        assert!(!force_may_help(&result(true, 0, "done")));
        // With the toggle on, a timeout fails instead of running -L...
        let timed_out = result(false, -1, "(timed out after 300s)");
        assert_eq!(decide_escalation(&timed_out, true, true, false), Escalation::Fail);
        // ...even when the log is dirty, the veto wins in auto mode.
        let dirty_io = result(false, 2, "metadata changes in a log ... pread: Input/output error");
        assert_eq!(decide_escalation(&dirty_io, true, true, false), Escalation::Fail);
        // A dirty log whose replay mount hit device errors or timed out: no -L either.
        let dirty = result(false, 2, "ERROR: The filesystem has valuable metadata changes in a log which needs to be replayed.");
        assert_eq!(decide_escalation(&dirty, true, true, true), Escalation::Fail);
        // Thursday's case: dirty log, replay failed on corruption → -L when allowed.
        assert_eq!(decide_escalation(&dirty, true, true, false), Escalation::RunForce);
        assert_eq!(decide_escalation(&dirty, true, false, false), Escalation::StopNeedsForce);
        // Interactive keeps asking the user for a dirty log.
        assert_eq!(decide_escalation(&dirty, false, false, false), Escalation::StopNeedsForce);
        assert_eq!(decide_escalation(&result(true, 0, ""), true, true, true), Escalation::Proceed);
    }

    #[test]
    fn sd_card_data_partition_is_repairable_but_system_partitions_are_not() {
        let mounts: std::collections::HashMap<String, String> = [
            ("/", "179:2"), ("/boot/firmware", "179:1"), ("/mutable", "179:4"),
        ].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let swaps = "Filename	Type	Size	Used	Priority
";
        // SD-only footage partition on the same card as root.
        assert_eq!(eligibility_from("179:3", &mounts, swaps), Ok(()));
        // External SSD partition.
        assert_eq!(eligibility_from("8:2", &mounts, swaps), Ok(()));
        for protected in ["179:2", "179:1", "179:4"] {
            assert!(eligibility_from(protected, &mounts, swaps).is_err(), "{protected}");
        }
        // Unknown root → fail closed.
        let no_root: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        assert!(eligibility_from("179:3", &no_root, swaps).is_err());
        // A swap entry that can't be identified → fail closed.
        let bad_swap = "Filename	Type	Size	Used	Priority
/nonexistent/swap partition 1 0 -2
";
        assert!(eligibility_from("179:3", &mounts, bad_swap).is_err());
    }

    #[test]
    fn a_repair_saved_as_running_is_interrupted_after_a_restart() {
        let base = RepairRecord { phase: "running".into(), device: Some("/dev/sda2".into()), started: 1, updated: 2, ..Default::default() };
        let after = after_restart(base.clone(), Some("b1")).unwrap();
        assert_eq!(after.phase, "interrupted");
        assert!(after.message.unwrap().contains("Run Repair Storage again"));
        // "Destructive step needed" survives restarts.
        let force = RepairRecord { phase: "needs_force".into(), ..base.clone() };
        assert_eq!(after_restart(force.clone(), Some("b1")), Some(force));
        // A reboot after a successful repair is finished business...
        let rebooting = RepairRecord { phase: "rebooting".into(), boot_id: Some("b1".into()), ..base };
        assert_eq!(after_restart(rebooting.clone(), Some("b2")), None);
        // ...unless the same boot is still running: only the service restarted.
        let same = after_restart(rebooting, Some("b1")).unwrap();
        assert_eq!(same.phase, "interrupted");
        assert!(same.message.unwrap().contains("Reboot the Pi"));
    }

    #[test]
    fn marker_roundtrip() {
        let path = std::env::temp_dir()
            .join(format!("sentryusb_marker_test_{}", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        clear_marker(&path); // clean slate even after a failed prior run
        assert!(!marker_exists(&path));
        // A temp dir isn't its own mount, so the guarded writer refuses it.
        assert!(!write_marker(&path));
        assert!(!marker_exists(&path));
        write_atomic(&path, b"{}").unwrap();
        assert!(marker_exists(&path));
        clear_marker(&path);
        assert!(!marker_exists(&path));
        clear_marker(&path); // idempotent on missing file
    }

    #[test]
    fn needs_log_replay_matches_xfs_repair_phrases() {
        // The exact ERROR xfs_repair prints when the log must be replayed.
        let replay = "ERROR: The filesystem has valuable metadata changes in a log which needs to\nbe replayed. Mount the filesystem to replay the log, and unmount it before\nre-running xfs_repair. If you are unable to mount the filesystem, then use\nthe -L option to destroy the log and attempt a repair.";
        assert!(needs_log_replay(replay));
        // A clean repair run must not trip the gate.
        let clean = "Phase 1 - find and verify superblock...\nPhase 7 - verify and correct link counts...\ndone";
        assert!(!needs_log_replay(clean));
    }
}

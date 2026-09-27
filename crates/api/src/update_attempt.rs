//! Crash-durable update receipts. Never automatically replay an installation.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Write, path::{Path, PathBuf}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub schema: u32,
    pub id: String,
    pub boot_id: String,
    pub cycle_id: String,
    pub source: String,
    pub target: String,
    pub phase: String,
    pub binary_sha256: String,
    pub detail: String,
    pub delivered: BTreeSet<String>,
    pub history_recorded: bool,
    #[serde(default)]
    pub verifying_boot_id: String,
    #[serde(default)]
    pub timestamp: i64,
}

impl Attempt {
    pub fn new(boot: &str, cycle: &str, source: &str, target: &str) -> Self {
        Self { schema: 1, id: format!("auto-update-{}-{}", chrono::Utc::now().timestamp_millis(), std::process::id()),
            boot_id: boot.into(), cycle_id: cycle.into(), source: source.into(), target: target.into(),
            phase: "preparing".into(), binary_sha256: String::new(), detail: String::new(),
            delivered: BTreeSet::new(), history_recorded: false, verifying_boot_id: String::new(), timestamp: chrono::Utc::now().timestamp() }
    }
    pub fn terminal(&self) -> bool { matches!(self.phase.as_str(), "verified" | "failed" | "interrupted") }
    pub fn boot_outcome(&self, boot: &str, binary_matches: bool, migration_ok: bool) -> &'static str {
        if self.phase != "verifying" || self.verifying_boot_id != boot || self.boot_id == boot || !binary_matches { "interrupted" }
        else if !migration_ok { "failed" } else { "verified" }
    }
    pub fn summary(&self) -> String {
        match self.phase.as_str() {
            "verified" => format!("Updated to {} successfully.", self.target),
            "interrupted" => "Update was interrupted. Check Notifications for details.".into(),
            _ if self.target.is_empty() => "Automatic update check failed. Check Notifications for details.".into(),
            _ => format!("Update to {} failed. Check Notifications for details.", self.target),
        }
    }
}

pub fn directory() -> PathBuf { PathBuf::from(sentryusb_config::mutable_dir()).join("sentryusb-updates") }
pub fn current_path() -> PathBuf { directory().join("attempt.json") }

/// An explicit manual repair can supersede an unreadable automatic receipt.
/// Preserve its exact bytes first; leave the live receipt blocking automatic
/// installation until the manual installer has actually succeeded.
pub fn preserve_before_manual_repair(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut original = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let backup = path.with_extension(format!("manual-repair-{}.json", rand::random::<u64>()));
    let mut saved = std::fs::OpenOptions::new().write(true).create_new(true).open(&backup)?;
    std::io::copy(&mut original, &mut saved)?;
    saved.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or_else(|| anyhow::anyhow!("Missing receipt directory"))?)?.sync_all()?;
    Ok(Some(backup))
}

pub fn clear_after_manual_repair(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            #[cfg(unix)]
            std::fs::File::open(path.parent().ok_or_else(|| anyhow::anyhow!("Missing receipt directory"))?)?.sync_all()?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Consume the expected-shutdown evidence before migration starts. A crash
/// during migration leaves `verifying` and is never resumed on another boot.
pub fn begin_verification(path: &Path, boot: &str) -> anyhow::Result<Attempt> {
    let mut a = load(path)?.ok_or_else(|| anyhow::anyhow!("No update attempt"))?;
    anyhow::ensure!(a.phase == "awaiting_reboot" && a.boot_id != boot, "Update did not complete its expected reboot");
    let receipt = path.with_file_name(format!("{}.shutdown.json", a.id));
    let proof: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt)?)?;
    anyhow::ensure!(proof["id"] == a.id && proof["boot_id"] == a.boot_id, "Invalid expected-shutdown receipt");
    a.phase = "verifying".into();
    a.verifying_boot_id = boot.into();
    save(path, &a)?;
    Ok(a)
}
pub fn load(path: &Path) -> anyhow::Result<Option<Attempt>> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let a: Attempt = serde_json::from_slice(&data)?;
    anyhow::ensure!(a.id.starts_with("auto-update-") && a.id.len() < 128 && a.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "Invalid update attempt ID");
    anyhow::ensure!(a.schema == 1, "unsupported update receipt schema");
    anyhow::ensure!(matches!(a.phase.as_str(), "preparing"|"installing"|"awaiting_reboot"|"verifying"|"verified"|"failed"|"interrupted"), "unknown update phase");
    Ok(Some(a))
}
pub fn save(path: &Path, attempt: &Attempt) -> anyhow::Result<()> {
    load(path)?; // Do not discard malformed existing evidence.
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension(format!("{}.{}.tmp", std::process::id(), rand::random::<u64>()));
    let result = (|| -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        f.write_all(&serde_json::to_vec(attempt)?)?;
        f.sync_all()?;
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_manual_repair_preserves_invalid_receipt_without_enabling_auto_replay() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("attempt.json");
        std::fs::write(&p, "damaged original receipt").unwrap();
        assert!(load(&p).is_err());
        let backup = preserve_before_manual_repair(&p).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "damaged original receipt");
        assert!(load(&p).is_err(), "keep blocking auto updates until manual installation succeeds");
        clear_after_manual_repair(&p).unwrap(); // successful explicit install supersedes it
        assert!(load(&p).unwrap().is_none());
        assert!(backup.exists());
    }
    #[test]
    fn unfinished_attempt_never_becomes_success_from_version_alone() {
        let mut a = Attempt::new("boot-a", "cycle-a", "v3.10.22", "v4.0.0");
        assert_eq!(a.boot_outcome("boot-b", true, true), "interrupted");
        a.phase = "awaiting_reboot".into();
        assert_eq!(a.boot_outcome("boot-b", true, true), "interrupted", "expected reboot flag alone is not shutdown evidence");
        a.phase = "verifying".into();
        a.verifying_boot_id = "boot-b".into();
        assert_eq!(a.boot_outcome("boot-b", false, true), "interrupted");
        assert_eq!(a.boot_outcome("boot-a", true, true), "interrupted");
        assert_eq!(a.boot_outcome("boot-b", true, false), "failed");
        assert_eq!(a.boot_outcome("boot-b", true, true), "verified");
    }
    #[test]
    fn durable_store_does_not_replace_corrupt_or_future_state() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("attempt.json");
        assert!(load(&p).unwrap().is_none());
        let a = Attempt::new("a", "c", "v1.0.0", "v2.0.0");
        save(&p, &a).unwrap();
        assert_eq!(load(&p).unwrap().unwrap().target, "v2.0.0");
        let mut receipt = a.clone();
        receipt.delivered.insert("pushover".into());
        save(&p, &receipt).unwrap();
        let recovered = load(&p).unwrap().unwrap();
        assert!(recovered.delivered.contains("pushover"));
        assert!(!recovered.delivered.contains("webhook"));
        assert_eq!(recovered.timestamp, a.timestamp);
        std::fs::write(&p, "bad").unwrap();
        assert!(load(&p).is_err());
        assert!(save(&p, &a).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bad");
    }
    #[test]
    fn shutdown_receipt_is_required_and_verification_is_bound_to_one_boot() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("attempt.json");
        let mut a = Attempt::new("old-boot", "cycle", "v1.0.0", "v2.0.0");
        a.phase = "awaiting_reboot".into(); save(&p, &a).unwrap();
        assert!(begin_verification(&p, "new-boot").is_err());
        std::fs::write(p.with_file_name(format!("{}.shutdown.json", a.id)),
            serde_json::to_vec(&serde_json::json!({"id":a.id,"boot_id":"old-boot"})).unwrap()).unwrap();
        let verifying = begin_verification(&p, "new-boot").unwrap();
        assert_eq!(verifying.boot_outcome("new-boot", true, true), "verified");
        assert_eq!(verifying.boot_outcome("another-boot", true, true), "interrupted");
        assert!(begin_verification(&p, "another-boot").is_err(), "must not replay interrupted startup migration");
    }
}

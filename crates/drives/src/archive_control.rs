//! One-shot archive cancellation shared with run/archive-control.sh.
//! The request contains a cycle ID, so it cannot disable a future visit.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static BACKGROUND_WORK: AtomicUsize = AtomicUsize::new(0);

/// Detached export/upload workers retain this guard until their writes stop.
/// Dropping an HTTP request must not falsely report the archive as quiescent.
pub struct ArchiveWorkGuard;

impl ArchiveWorkGuard {
    pub fn begin() -> Self {
        BACKGROUND_WORK.fetch_add(1, Ordering::SeqCst);
        Self
    }

    pub fn is_running() -> bool {
        BACKGROUND_WORK.load(Ordering::SeqCst) != 0
    }
}

impl Drop for ArchiveWorkGuard {
    fn drop(&mut self) { BACKGROUND_WORK.fetch_sub(1, Ordering::SeqCst); }
}

#[derive(Clone)]
pub struct ArchiveControl {
    directory: PathBuf,
    cleanup_deferred: PathBuf,
}

impl Default for ArchiveControl {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("/tmp"),
            cleanup_deferred: PathBuf::from("/mutable/sentryusb_cam_cleanup_deferred"),
        }
    }
}

impl ArchiveControl {
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            cleanup_deferred: directory.as_ref().join("sentryusb_cam_cleanup_deferred"),
        }
    }

    pub fn active_cycle(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.directory.join("archive-cycle")).ok()?;
        let id = id.trim();
        let (pid, nonce) = id.split_once(':')?;
        let pid: u32 = pid.parse().ok()?;
        if nonce.is_empty() || id.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        // An abrupt service exit must not leave a phantom Cancel button.
        #[cfg(target_os = "linux")]
        if !Path::new(&format!("/proc/{pid}")).exists() { return None; }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        Some(id.to_owned())
    }

    pub fn cancelled(&self) -> bool {
        let Some(id) = self.active_cycle() else { return false; };
        self.cycle_cancelled(&id)
    }

    pub fn cycle_cancelled(&self, id: &str) -> bool {
        self.directory.join(format!("archive-cycle-cancel-{id}")).exists()
    }

    /// Hold the returned guard until companion workers have been notified.
    /// The shell cannot close this cycle and start another while it is held.
    pub fn request_cancel(&self, expected_cycle: &str) -> io::Result<crate::archive_mount_lock::ArchiveMountGuard> {
        // Shared with shell cycle-close and cleanup-deferral release. An
        // accepted request cannot race past cycle close or lose its durable
        // protection to a simultaneously successful transfer.
        let guard = crate::archive_mount_lock::acquire_path(
            &self.directory.join("archive-cycle.lock"),
            std::time::Duration::from_secs(5),
        )?;
        if self.active_cycle().as_deref() != Some(expected_cycle) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "archive cycle has already ended"));
        }
        // Persist protection BEFORE the volatile request is visible or HTTP
        // 202 is returned. File and directory fsync also cover sudden power loss.
        let persist = || -> io::Result<()> {
            let file = std::fs::OpenOptions::new()
                .write(true).create(true).truncate(false).open(&self.cleanup_deferred)?;
            file.sync_all()?;
            #[cfg(unix)]
            std::fs::File::open(self.cleanup_deferred.parent().unwrap())?.sync_all()?;
            Ok(())
        };
        persist().map_err(|e| io::Error::other(format!("cannot protect footage cleanup: {e}")))?;
        // Different cycles have different files. A request delayed across a
        // cycle transition cannot overwrite the newer cycle's cancellation.
        std::fs::write(self.directory.join(format!("archive-cycle-cancel-{expected_cycle}")), b"")?;
        Ok(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn shell_close_waits_for_accepted_request_then_captures_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(root.path());
        let id = format!("{}:closing", std::process::id());
        std::fs::write(root.path().join("archive-cycle"), &id).unwrap();
        let guard = control.request_cancel(&id).unwrap();
        let script = format!("{}\narchive_cycle_end\ntest \"$ARCHIVE_CYCLE_CANCELLED\" = 1",
            include_str!("../../../run/archive-control.sh"));
        let mut shell = std::process::Command::new("bash")
            .args(["-eu", "-c", &script])
            .env("ARCHIVE_CONTROL_DIR", root.path())
            .env("ARCHIVE_CYCLE_ID", &id)
            .env("ARCHIVE_CYCLE_CANCELLED", "0")
            .spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(shell.try_wait().unwrap().is_none(), "close raced accepted cancellation");
        assert!(control.active_cycle().is_some());
        drop(guard);
        assert!(shell.wait().unwrap().success());
        assert!(control.active_cycle().is_none());
        assert!(control.request_cancel(&id).is_err());
    }

    #[test]
    fn acknowledged_cancel_protects_cleanup_even_without_the_shell() {
        let root = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(root.path());
        let id = format!("{}:power-loss", std::process::id());
        std::fs::write(root.path().join("archive-cycle"), &id).unwrap();
        control.request_cancel(&id).unwrap();
        std::fs::remove_file(root.path().join("archive-cycle")).unwrap();
        std::fs::remove_file(root.path().join(format!("archive-cycle-cancel-{id}"))).unwrap();
        assert!(root.path().join("sentryusb_cam_cleanup_deferred").is_file());
    }

    #[test]
    fn cannot_accept_cancel_when_cleanup_protection_cannot_be_persisted() {
        let root = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(root.path());
        let id = format!("{}:write-failure", std::process::id());
        std::fs::write(root.path().join("archive-cycle"), &id).unwrap();
        std::fs::create_dir(root.path().join("sentryusb_cam_cleanup_deferred")).unwrap();
        assert!(control.request_cancel(&id).is_err());
        assert!(!control.cancelled());
    }

    #[test]
    fn cancellation_is_idempotent_and_never_carries_into_the_next_visit() {
        let root = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(root.path());
        let first = format!("{}:first", std::process::id());
        let next = format!("{}:next", std::process::id());
        assert!(control.request_cancel(&first).is_err());
        std::fs::write(root.path().join("archive-cycle"), &first).unwrap();
        control.request_cancel(&first).unwrap();
        control.request_cancel(&first).unwrap();
        assert!(control.cancelled());
        std::fs::remove_file(root.path().join("archive-cycle")).unwrap();
        assert!(!control.cancelled());
        std::fs::write(root.path().join("archive-cycle"), &next).unwrap();
        assert!(!control.cancelled());
        assert!(control.request_cancel(&first).is_err());
        assert!(!control.cancelled());
        control.request_cancel(&next).unwrap();
        // Simulate an A request that validated before B, then published late.
        std::fs::write(root.path().join(format!("archive-cycle-cancel-{first}")), b"").unwrap();
        assert!(control.cancelled(), "late A cannot erase B's request");
    }
}

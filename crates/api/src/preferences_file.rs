use std::{fs, io::{ErrorKind, Write}, path::Path, sync::atomic::{AtomicU64, Ordering}};
use anyhow::{Context, Result};
use serde_json::{Map, Value};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

// Only an absent current file permits the legacy fallback. An unreadable or
// malformed current document must never become an empty writable baseline.
pub(super) fn load(primary: &Path, legacy: &Path) -> Result<Map<String, Value>> {
    fn read(path: &Path) -> Result<Option<Map<String, Value>>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).context("invalid preferences document")?)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("read preferences"),
        }
    }
    if let Some(value) = read(primary)? { return Ok(value) }
    Ok(read(legacy)?.unwrap_or_default())
}

pub(super) fn save(path: &Path, prefs: &Map<String, Value>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(prefs)?;
    let parent = path.parent().context("preferences path has no parent")?;
    fs::create_dir_all(parent).context("create preferences directory")?;
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let filename = path.file_name().context("preferences path has no filename")?.to_string_lossy();
    let temporary = parent.join(format!(".{filename}.{}.{}.tmp", std::process::id(), id));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)
        .context("create preferences temporary file")?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes).context("write preferences")?;
        file.sync_all().context("flush preferences")?;
        drop(file);
        fs::rename(&temporary, path).context("publish preferences")?;
        fs::File::open(parent).and_then(|directory| directory.sync_all())
            .context("flush preferences directory")?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_is_used_only_when_current_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("current.json"); let legacy = dir.path().join("legacy.json");
        assert!(load(&primary, &legacy).unwrap().is_empty());
        fs::write(&legacy, br#"{"charging_currency":"CAD"}"#).unwrap();
        assert_eq!(load(&primary, &legacy).unwrap()["charging_currency"], "CAD");
        fs::write(&primary, br#"{"charging_currency":"USD"}"#).unwrap();
        assert_eq!(load(&primary, &legacy).unwrap()["charging_currency"], "USD");
        fs::write(&primary, b"broken").unwrap();
        assert!(load(&primary, &legacy).is_err());
        assert_eq!(fs::read(&primary).unwrap(), b"broken");
    }

    #[test]
    fn malformed_or_unreadable_documents_are_not_empty_preferences() {
        let dir = tempfile::tempdir().unwrap(); let primary = dir.path().join("current.json"); let legacy = dir.path().join("legacy.json");
        for body in [b"[]".as_slice(), b"null", b"\xff", b"{\"key\":"] {
            fs::write(&primary, body).unwrap();
            assert!(load(&primary, &legacy).is_err());
        }
        fs::remove_file(&primary).unwrap(); fs::create_dir(&primary).unwrap();
        assert!(load(&primary, &legacy).is_err());
        fs::remove_dir(&primary).unwrap(); fs::write(&legacy, b"broken").unwrap();
        assert!(load(&primary, &legacy).is_err());
    }

    #[test]
    fn failed_publication_preserves_existing_target_and_cleans_only_our_temp() {
        let dir = tempfile::tempdir().unwrap(); let primary = dir.path().join("current.json");
        fs::create_dir(&primary).unwrap(); fs::write(primary.join("keep"), b"untouched").unwrap();
        let unrelated = dir.path().join("unrelated.tmp"); fs::write(&unrelated, b"untouched").unwrap();
        assert!(save(&primary, json!({"charging_default_rate":0.15}).as_object().unwrap()).is_err());
        assert_eq!(fs::read(primary.join("keep")).unwrap(), b"untouched");
        assert_eq!(fs::read(&unrelated).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn checked_save_roundtrips_the_complete_map() {
        let dir = tempfile::tempdir().unwrap(); let primary = dir.path().join("nested/current.json");
        let prefs = json!({"charging_default_rate":0.15,"charging_currency":"CAD","unrelated":{"keep":true}});
        save(&primary, prefs.as_object().unwrap()).unwrap();
        assert_eq!(load(&primary, &dir.path().join("missing")).unwrap(), prefs.as_object().unwrap().clone());
        assert_eq!(fs::read_dir(primary.parent().unwrap()).unwrap().count(), 1);
    }
}

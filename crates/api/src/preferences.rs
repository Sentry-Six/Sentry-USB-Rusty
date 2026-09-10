//! User preferences (key-value store).
//!
//! Load/modify/save operations share a process-wide lock, and writes use an
//! atomic temporary-file rename.

use std::sync::Mutex;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::router::AppState;

#[path = "preferences_file.rs"]
mod file_store;
#[path="preferences_journal.rs"]
mod journal;
#[path = "charging_rates_editor.rs"]
pub mod rates_editor;

/// Preferences path beneath the configured mutable directory.
pub(crate) fn prefs_file() -> String {
    format!("{}/.sentryusb_preferences.json", sentryusb_config::mutable_dir())
}
/// Legacy Go preferences path — read-only fallback so upgrades don't lose data.
fn legacy_prefs_file() -> String {
    format!("{}/sentryusb-prefs.json", sentryusb_config::mutable_dir())
}

/// Serializes preference read/modify/write operations.
static PREFS_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn load_prefs() -> serde_json::Map<String, serde_json::Value> {
    // Preserve the previous filename as a read-only migration fallback.
    if let Ok(d) = std::fs::read_to_string(prefs_file()) {
        if let Ok(v) = serde_json::from_str(&d) {
            return v;
        }
    }
    std::fs::read_to_string(legacy_prefs_file())
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default()
}

pub(crate) fn load_prefs_checked(store:&sentryusb_drives::DriveStore) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let _guard=PREFS_LOCK.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    journal::recover(std::path::Path::new(&prefs_file()),std::path::Path::new(&legacy_prefs_file()),store)
}

/// Preserve the exact Home-freeze label without asserting unrelated rates.
pub(crate) fn update_rate_plan<F>(store:&sentryusb_drives::DriveStore,tag:&str,f:F)->anyhow::Result<bool>
where F:FnOnce(&mut serde_json::Map<String,serde_json::Value>)->anyhow::Result<bool> {
    update_rate_plan_at(std::path::Path::new(&prefs_file()),std::path::Path::new(&legacy_prefs_file()),store,tag,f)
}
fn update_rate_plan_at<F>(primary:&std::path::Path,legacy:&std::path::Path,store:&sentryusb_drives::DriveStore,tag:&str,f:F)->anyhow::Result<bool>
where F:FnOnce(&mut serde_json::Map<String,serde_json::Value>)->anyhow::Result<bool> {
    let _guard=PREFS_LOCK.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    let mut prefs=journal::recover(primary,legacy,store)?;
    let before=prefs.clone();
    if !f(&mut prefs)? {return Ok(false)}
    journal::publish_with_tag(primary,store,&before,&prefs,false,Some(tag))?;
    Ok(true)
}

/// All preference writers merge into the latest checked document while locked.
/// Unrelated edits must not restore an older copy of charging-rate preferences.
pub(crate) fn edit_prefs<F>(store:&sentryusb_drives::DriveStore,f:F)->anyhow::Result<bool>
where F:FnOnce(&mut serde_json::Map<String,serde_json::Value>)->anyhow::Result<bool> {
    edit_prefs_at(std::path::Path::new(&prefs_file()),std::path::Path::new(&legacy_prefs_file()),store,false,f)
}
fn rate_fields(prefs:&serde_json::Map<String,serde_json::Value>)->serde_json::Map<String,serde_json::Value> {
    RATE_CONFIG_KEYS.iter().filter_map(|key|prefs.get(*key).map(|value|((*key).to_string(),value.clone()))).collect()
}
fn edit_prefs_at<F>(primary:&std::path::Path,legacy:&std::path::Path,store:&sentryusb_drives::DriveStore,force_rates:bool,f:F)->anyhow::Result<bool>
where F:FnOnce(&mut serde_json::Map<String,serde_json::Value>)->anyhow::Result<bool> {
    let _guard=PREFS_LOCK.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    let mut prefs=journal::recover(primary,legacy,store)?;
    let before=prefs.clone();
    if !f(&mut prefs)? {return Ok(false)}
    if force_rates || rate_fields(&before)!=rate_fields(&prefs) {
        journal::publish(primary,store,&before,&prefs,force_rates)?;
    } else {file_store::save(primary,&prefs)?;}
    Ok(true)
}

#[derive(Deserialize)]
pub struct PrefQuery {
    key: Option<String>,
}

/// GET /api/config/preference
pub async fn get_preference(
    State(state): State<AppState>,
    Query(params): Query<PrefQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let store=state.drives.store.clone();
    let prefs=match tokio::task::spawn_blocking(move||load_prefs_checked(&store)).await {
        Ok(Ok(prefs))=>prefs,
        _=>return crate::json_error(StatusCode::INTERNAL_SERVER_ERROR,"Preferences could not be read."),
    };
    if let Some(key) = &params.key {
        let val = prefs.get(key).cloned().unwrap_or(serde_json::Value::Null);
        (StatusCode::OK, Json(serde_json::json!({"key": key, "value": val})))
    } else {
        (StatusCode::OK, Json(serde_json::Value::Object(prefs)))
    }
}

/// PUT /api/config/preference
pub async fn set_preference(
    State(s): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    #[derive(Deserialize)]
    struct SetReq {
        key: String,
        value: serde_json::Value,
    }

    let req: SetReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return crate::json_error(StatusCode::BAD_REQUEST, "invalid request body"),
    };
    let key = req.key.clone();

    let store=s.drives.store.clone();
    let saved=tokio::task::spawn_blocking(move||edit_prefs_at(std::path::Path::new(&prefs_file()),std::path::Path::new(&legacy_prefs_file()),
        &store,false,|prefs| {
            prefs.insert(req.key,req.value);
            Ok(true)
        })).await.map_err(anyhow::Error::from).and_then(|result|result);
    if RATE_CONFIG_KEYS.contains(&key.as_str()) { s.cloud.uploader.nudge(); }
    if let Err(error) = saved {
        tracing::warn!("[preferences] save not confirmed: {error}");
        return crate::json_error(StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to confirm preference save. Reload and try again.");
    }
    crate::json_ok()
}

/// The preference keys that make up the per-Pi charging rate-config
/// document synced to/from the cloud (charging.rs `RateConfig::load`
/// reads exactly these).
pub const RATE_CONFIG_KEYS:&[&str]=sentryusb_drives::mutable_intent::RATE_KEYS;

/// Cloud rate-config access restricted to [`RATE_CONFIG_KEYS`].
pub struct PrefsRateConfig;

impl sentryusb_cloud_uploader::RateConfigAccess for PrefsRateConfig {
    fn home_config_sync_enabled(&self)->bool {true}
    fn home_geofence(&self)->anyhow::Result<Option<sentryusb_drives::home::HomeGeofence>> {
        crate::charging::checked_home_geofence()
    }
    fn load_doc(&self,store:&sentryusb_drives::DriveStore) -> anyhow::Result<serde_json::Value> {
        let prefs = load_prefs_checked(store)?;
        let mut doc = serde_json::Map::new();
        for k in RATE_CONFIG_KEYS {
            if let Some(v) = prefs.get(*k) {
                doc.insert((*k).to_string(), v.clone());
            }
        }
        Ok(serde_json::Value::Object(doc))
    }

    fn confirm_doc(&self, doc:&serde_json::Value, store:&sentryusb_drives::DriveStore, through:i64,
        receipt:Option<(&str,&str)>) -> anyhow::Result<()> {
        let _guard = PREFS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        journal::confirm(std::path::Path::new(&prefs_file()), std::path::Path::new(&legacy_prefs_file()), store, doc, through, receipt)
    }

    fn queue_initial_doc(&self,store:&sentryusb_drives::DriveStore)->anyhow::Result<()> {
        let _guard=PREFS_LOCK.lock().unwrap_or_else(|p|p.into_inner());
        queue_initial_rates(std::path::Path::new(&prefs_file()),std::path::Path::new(&legacy_prefs_file()),store)
    }

    fn store_doc(&self, doc: &serde_json::Value, store: &sentryusb_drives::DriveStore, updated_at_ms: i64) -> anyhow::Result<()> {
        let _guard = PREFS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        store_rate_doc(std::path::Path::new(&prefs_file()), std::path::Path::new(&legacy_prefs_file()), doc, store, updated_at_ms)
    }
}

// The caller holds PREFS_LOCK through recovery, selection and queue reservation.
fn queue_initial_rates(primary:&std::path::Path,legacy:&std::path::Path,
    store:&sentryusb_drives::DriveStore)->anyhow::Result<()> {
    let prefs=journal::recover(primary,legacy,store)?;
    let doc=serde_json::Value::Object(RATE_CONFIG_KEYS.iter()
        .filter_map(|key|prefs.get(*key).map(|value|((*key).to_string(),value.clone()))).collect());
    store.queue_initial_rate_config(&doc)?;
    Ok(())
}

// The caller holds PREFS_LOCK across the queued-generation check and file save.
fn store_rate_doc(primary: &std::path::Path, legacy: &std::path::Path,
    doc: &serde_json::Value, store: &sentryusb_drives::DriveStore, _updated_at_ms: i64,
) -> anyhow::Result<()> {
    let obj = doc.as_object().ok_or_else(|| anyhow::anyhow!("rate config doc is not an object"))?;
    let mut prefs=journal::recover(primary,legacy,store)?;
    let pending = store.dirty_mutables()?.into_iter().find(|(kind, key, _)| kind == "rate" && key.is_empty());
    // Pending local field intent must be merged by the outgoing writer; a
    // newer whole-document timestamp cannot discard it here.
    if pending.is_some() {return Ok(())}
    for key in RATE_CONFIG_KEYS {
        match obj.get(*key) {
            Some(value) => { prefs.insert((*key).to_string(), value.clone()); }
            None => { prefs.remove(*key); }
        }
    }
    file_store::save(primary, &prefs)?;
    crate::charging::invalidate_charging_list();
    Ok(())
}

#[cfg(test)]
mod rate_sync_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_rates_are_durable_without_changing_preferences_or_copying_private_fields() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");
        let legacy=dir.path().join("legacy.json");let db=dir.path().join("drive.db");
        let prefs=json!({"charging_currency":"CAD","charging_tag_rates":{"Home":0.12},
            "home_lat":12.34,"private_token":"synthetic-only"});
        file_store::save(&file,prefs.as_object().unwrap()).unwrap();
        let original=std::fs::read(&file).unwrap();
        {
            let store=sentryusb_drives::DriveStore::open(db.to_str().unwrap()).unwrap();
            queue_initial_rates(&file,&legacy,&store).unwrap();
            let first=store.rate_sync_snapshot().unwrap();
            queue_initial_rates(&file,&legacy,&store).unwrap();
            assert_eq!(store.rate_sync_snapshot().unwrap(),first);
            assert_eq!(std::fs::read(&file).unwrap(),original);
        }
        let store=sentryusb_drives::DriveStore::open(db.to_str().unwrap()).unwrap();
        let snapshot=store.rate_sync_snapshot().unwrap();assert!(!snapshot.intent.legacy);
        assert_eq!(snapshot.intent.edits.len(),1);
        let sentryusb_drives::mutable_intent::Edit::RateConfig {before,after,..}=&snapshot.intent.edits[0].1 else {panic!("wrong intent")};
        assert_eq!(*before,json!({}));
        assert_eq!(*after,json!({"charging_currency":"CAD","charging_tag_rates":{"Home":0.12}}));
        assert_eq!(std::fs::read(&file).unwrap(),original);
    }

    #[test]
    fn initial_queue_recovers_interrupted_preferences_before_reading_and_retains_that_edit() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");
        let legacy=dir.path().join("legacy.json");let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        let before=json!({"charging_default_rate":0.1,"private":"synthetic"});
        let after=json!({"charging_default_rate":0.2,"private":"synthetic"});
        file_store::save(&file,before.as_object().unwrap()).unwrap();
        // Leave the real file-publication journal pending by forcing its save
        // to fail after reservation, then repair the destination for recovery.
        std::fs::remove_file(&file).unwrap();std::fs::create_dir(&file).unwrap();
        assert!(journal::publish(&file,&store,before.as_object().unwrap(),after.as_object().unwrap(),false).is_err());
        let pending=store.rate_sync_snapshot().unwrap();
        std::fs::remove_dir(&file).unwrap();
        file_store::save(&file,before.as_object().unwrap()).unwrap();
        queue_initial_rates(&file,&legacy,&store).unwrap();
        assert_eq!(store.rate_sync_snapshot().unwrap(),pending);
        assert_eq!(file_store::load(&file,&legacy).unwrap(),after.as_object().unwrap().clone());
        assert!(store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,
            sentryusb_drives::mutable_intent::RATE_PREFERENCES_JOURNAL)).unwrap().is_none());
    }

    #[test]
    fn unreadable_initial_preferences_cannot_become_an_empty_upload() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");
        let legacy=dir.path().join("legacy.json");let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        std::fs::write(&file,"{broken").unwrap();
        assert!(queue_initial_rates(&file,&legacy,&store).is_err());
        assert!(store.dirty_mutables().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(),"{broken");
    }

    #[test]
    fn incoming_rates_preserve_all_pending_local_intent_and_unrelated_preferences() {
        let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("prefs.json"); let legacy = dir.path().join("legacy.json");
        let store = sentryusb_drives::DriveStore::open_memory().unwrap();
        file_store::save(&file, json!({"charging_currency":"CAD", "charging_default_rate":0.2, "unrelated":true}).as_object().unwrap()).unwrap();
        store.mark_rate_config_dirty().unwrap(); let at = store.dirty_mutables().unwrap()[0].2;
        store_rate_doc(&file, &legacy, &json!({"charging_currency":"USD"}), &store, at).unwrap();
        assert_eq!(file_store::load(&file, &legacy).unwrap()["charging_currency"], "CAD");
        assert_eq!(store.dirty_mutables().unwrap().len(), 1);
        store_rate_doc(&file, &legacy, &json!({"charging_currency":"USD"}), &store, at+1).unwrap();
        assert_eq!(file_store::load(&file,&legacy).unwrap()["charging_currency"],"CAD");
        assert_eq!(store.dirty_mutables().unwrap().len(),1);
        store.clear_mutable_dirty("rate","",at).unwrap();
        store_rate_doc(&file,&legacy,&json!({"charging_currency":"USD"}),&store,at+1).unwrap();
        let prefs = file_store::load(&file, &legacy).unwrap();
        assert_eq!(prefs["charging_currency"], "USD"); assert_eq!(prefs["unrelated"], true);
        assert!(!prefs.contains_key("charging_default_rate")); assert!(store.dirty_mutables().unwrap().is_empty());
    }

    #[test]
    fn incoming_rate_failure_does_not_clear_the_pending_generation() {
        let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("prefs.json"); let legacy = dir.path().join("legacy.json");
        let store = sentryusb_drives::DriveStore::open_memory().unwrap(); store.mark_rate_config_dirty().unwrap();
        let at = store.dirty_mutables().unwrap()[0].2;
        std::fs::write(&file, b"broken").unwrap();
        std::fs::write(&legacy, br#"{"charging_currency":"CAD"}"#).unwrap();
        assert!(store_rate_doc(&file, &legacy, &json!({"charging_currency":"USD"}), &store, at+1).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"broken"); assert_eq!(store.dirty_mutables().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc,mpsc};

    #[test]
    fn concurrent_unrelated_writer_reads_the_latest_rate_values_under_the_same_lock() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
        file_store::save(&file,json!({"charging_currency":"USD","keep":true}).as_object().unwrap()).unwrap();
        let store=Arc::new(sentryusb_drives::DriveStore::open_memory().unwrap());
        let (ready,wait_ready)=mpsc::channel();let (release,wait_release)=mpsc::channel();
        let (started,wait_started)=mpsc::channel();
        let first_file=file.clone();let first_legacy=legacy.clone();let first_store=store.clone();
        let first=std::thread::spawn(move|| {
            edit_prefs_at(&first_file,&first_legacy,&first_store,false,|prefs| {
                ready.send(()).unwrap();wait_release.recv().unwrap();
                prefs.insert("charging_currency".into(),json!("CAD"));Ok(true)
            }).unwrap();
            first_store.dirty_mutables().unwrap()[0].2
        });
        wait_ready.recv().unwrap();let second_file=file.clone();let second_legacy=legacy.clone();let second_store=store.clone();
        let second=std::thread::spawn(move|| {
            started.send(()).unwrap();
            edit_prefs_at(&second_file,&second_legacy,&second_store,false,|prefs| {
                assert_eq!(prefs["charging_currency"],"CAD");
                prefs.insert("notify_update".into(),json!(false));Ok(true)
            }).unwrap();
        });
        wait_started.recv().unwrap();release.send(()).unwrap();let generation=first.join().unwrap();second.join().unwrap();
        let saved=file_store::load(&file,&legacy).unwrap();
        assert_eq!(saved["charging_currency"],"CAD");assert_eq!(saved["notify_update"],false);assert_eq!(saved["keep"],true);
        assert_eq!(store.dirty_mutables().unwrap(),vec![("rate".into(),String::new(),generation)]);
    }

    #[test]
    fn malformed_current_preferences_never_become_a_writable_legacy_baseline() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
        std::fs::write(&file,b"broken").unwrap();std::fs::write(&legacy,br#"{"charging_currency":"CAD"}"#).unwrap();
        let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        assert!(edit_prefs_at(&file,&legacy,&store,false,|_|panic!("unreadable preferences reached an editor")).is_err());
        assert_eq!(std::fs::read(file).unwrap(),b"broken");assert!(store.dirty_mutables().unwrap().is_empty());
    }

    #[test]
    fn explicit_rate_republication_remains_queued_without_dirtying_on_unrelated_edits() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
        file_store::save(&file,json!({"charging_currency":"CAD"}).as_object().unwrap()).unwrap();
        let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        edit_prefs_at(&file,&legacy,&store,false,|prefs| {prefs.insert("notify_update".into(),json!(true));Ok(true)}).unwrap();
        assert!(store.dirty_mutables().unwrap().is_empty());
        edit_prefs_at(&file,&legacy,&store,true,|_|Ok(true)).unwrap();
        assert_eq!(store.dirty_mutables().unwrap().len(),1);
    }
}

#[cfg(test)]
mod recovery_integration_tests {
    use super::*;
    use serde_json::json;
    use sentryusb_drives::mutable_intent::{RATE_PREFERENCES_JOURNAL,Edit};
    fn pending_file(file:&std::path::Path,store:&sentryusb_drives::DriveStore) {
        let before=json!({"charging_currency":"CAD","charging_default_rate":0.2});
        let after=json!({"charging_currency":"CAD","charging_default_rate":0.3});
        file_store::save(file,before.as_object().unwrap()).unwrap();
        store.with_locked_conn(|conn|conn.execute_batch(&format!("CREATE TRIGGER fail_rate_cleanup BEFORE DELETE ON meta WHEN OLD.key='{RATE_PREFERENCES_JOURNAL}' BEGIN SELECT RAISE(ABORT,'synthetic interruption'); END;"))).unwrap();
        assert!(journal::publish(file,store,before.as_object().unwrap(),after.as_object().unwrap(),false).is_err());
        store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_rate_cleanup")).unwrap();
    }
    #[test]
    fn next_editor_recovers_the_previous_value_before_recording_new_field_intent() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
        let store=sentryusb_drives::DriveStore::open_memory().unwrap();pending_file(&file,&store);
        edit_prefs_at(&file,&legacy,&store,false,|prefs| {
            assert!(store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,RATE_PREFERENCES_JOURNAL)).unwrap().is_none());
            assert_eq!(prefs["charging_default_rate"],0.3);prefs.insert("charging_default_rate".into(),json!(0.4));Ok(true)
        }).unwrap();
        let intent=store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"rate","")).unwrap();
        assert!(!intent.legacy);assert_eq!(intent.edits.len(),2);
        let Edit::RateConfig {before,after,..}=&intent.edits[1].1 else {panic!("missing rate field intent")};
        assert_eq!(before["charging_default_rate"],0.3);assert_eq!(after["charging_default_rate"],0.4);
    }
    #[test]
    fn incoming_rates_recover_the_file_but_preserve_pending_local_fields() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
        let store=sentryusb_drives::DriveStore::open_memory().unwrap();pending_file(&file,&store);
        let queue=store.dirty_mutables().unwrap();
        store_rate_doc(&file,&legacy,&json!({"charging_currency":"USD","charging_default_rate":0.7}),&store,queue[0].2+10000).unwrap();
        let prefs=file_store::load(&file,&legacy).unwrap();assert_eq!(prefs["charging_currency"],"CAD");assert_eq!(prefs["charging_default_rate"],0.3);
        assert_eq!(store.dirty_mutables().unwrap(),queue);
        assert!(store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,RATE_PREFERENCES_JOURNAL)).unwrap().is_none());
    }
}

#[cfg(test)]
mod required_plan_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn existing_home_freeze_plan_records_only_its_named_requirement() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        let before=json!({"charging_currency":"CAD","charging_default_rate":0.1,"charging_tag_rates":{"Old Home":0.2,"Work":0.4}});
        file_store::save(&file,before.as_object().unwrap()).unwrap();
        update_rate_plan_at(&file,&legacy,&store,"Old Home",|_|Ok(true)).unwrap();
        let snapshot=store.rate_sync_snapshot().unwrap();assert_eq!(snapshot.intent.edits.len(),1);
        assert!(matches!(&snapshot.intent.edits[0].1,sentryusb_drives::mutable_intent::Edit::RateConfig {force:false,required_tag:Some(tag),..} if tag=="Old Home"));
        let cloud=json!({"charging_currency":"USD","charging_default_rate":0.3,"charging_tag_rates":{"Work":0.5}});
        let merged=sentryusb_drives::rate_intent::merge(&snapshot.intent,&cloud).unwrap();
        assert_eq!(merged["charging_currency"],"USD");assert_eq!(merged["charging_default_rate"],0.3);assert_eq!(merged["charging_tag_rates"]["Work"],0.5);
        assert_eq!(merged["charging_tag_rates"]["Old Home"]["flat"],0.2);
        assert_eq!(file_store::load(&file,&legacy).unwrap(),before.as_object().unwrap().clone());
    }
    #[test]
    fn missing_required_plan_never_writes_the_file_or_queues_an_edit() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        let before=json!({"charging_currency":"CAD"});file_store::save(&file,before.as_object().unwrap()).unwrap();
        assert!(update_rate_plan_at(&file,&legacy,&store,"Missing",|prefs| {prefs.insert("charging_currency".into(),json!("USD"));Ok(true)}).is_err());
        assert_eq!(file_store::load(&file,&legacy).unwrap(),before.as_object().unwrap().clone());assert!(store.dirty_mutables().unwrap().is_empty());
    }
    #[test]
    fn unchanged_single_rate_preference_does_not_create_an_assertion() {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let store=sentryusb_drives::DriveStore::open_memory().unwrap();
        file_store::save(&file,json!({"charging_currency":"CAD"}).as_object().unwrap()).unwrap();
        edit_prefs_at(&file,&legacy,&store,false,|prefs| {prefs.insert("charging_currency".into(),json!("CAD"));Ok(true)}).unwrap();
        assert!(store.dirty_mutables().unwrap().is_empty());
    }
}

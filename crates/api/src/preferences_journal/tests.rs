use super::*;
use serde_json::json;
fn map(value:Value)->Map<String,Value> {value.as_object().unwrap().clone()}
fn raw(primary:&Path,before:&Map<String,Value>,after:&Map<String,Value>)->String {
    serde_json::to_string(&Pending {version:1,path:primary.to_str().unwrap().into(),before_hash:hash(&rates(before)).unwrap(),
        after_hash:hash(&rates(after)).unwrap(),rates:rates(after),confirmation:None}).unwrap()
}
fn stage(primary:&Path,store:&DriveStore,before:&Map<String,Value>,after:&Map<String,Value>)->String {
    let raw=raw(primary,before,after);
    store.stage_rate_preferences(&Value::Object(rates(before)),&Value::Object(rates(after)),false,&raw).unwrap();raw
}
fn saved(store:&DriveStore)->Option<String> {store.with_locked_conn(|conn|schema::meta_get(conn,RATE_PREFERENCES_JOURNAL)).unwrap()}

#[test]
fn failed_queue_or_journal_storage_cannot_publish_the_file() {
    for table in ["mutable_dirty","meta"] {
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let before=map(json!({"charging_default_rate":0.2}));
        file_store::save(&file,&before).unwrap();let bytes=std::fs::read(&file).unwrap();
        let store=DriveStore::open_memory().unwrap();
        store.with_locked_conn(|conn|conn.execute_batch(&if table=="meta" {
            format!("CREATE TRIGGER fail_journal BEFORE INSERT ON meta WHEN NEW.key='{RATE_PREFERENCES_JOURNAL}' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")
        } else {"CREATE TRIGGER fail_queue BEFORE INSERT ON mutable_dirty BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;".into()})).unwrap();
        assert!(publish(&file,&store,&before,&map(json!({"charging_default_rate":0.3})),false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(),bytes);assert!(saved(&store).is_none());assert!(store.dirty_mutables().unwrap().is_empty());
        assert!(store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"rate","")).unwrap().edits.is_empty());
    }
}

#[test]
fn recovery_before_file_publication_preserves_unrelated_private_settings() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
    let before=map(json!({"charging_default_rate":0.2,"private_token":"synthetic-private-value","notify_update":true}));
    let after=map(json!({"charging_default_rate":0.3,"private_token":"synthetic-private-value","notify_update":false}));
    file_store::save(&file,&before).unwrap();let store=DriveStore::open_memory().unwrap();let raw=stage(&file,&store,&before,&after);
    assert!(!raw.contains("synthetic-private-value"));assert!(!raw.contains("private_token"));assert!(!raw.contains("notify_update"));
    let mut current=before.clone();current.insert("private_token".into(),json!("newer-private-value"));file_store::save(&file,&current).unwrap();
    let recovered=recover(&file,&legacy,&store).unwrap();
    assert_eq!(recovered["charging_default_rate"],0.3);assert_eq!(recovered["private_token"],"newer-private-value");
    assert_eq!(recovered["notify_update"],true,"the journal must not guess missing unrelated changes from an interrupted request");
    assert!(saved(&store).is_none());assert_eq!(store.dirty_mutables().unwrap().len(),1);
    let intent=store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"rate","")).unwrap();
    assert_eq!(intent.edits.len(),1);assert!(!serde_json::to_string(&intent).unwrap().contains("private_token"));
}

#[test]
fn recovery_after_rename_keeps_newer_unrelated_fields_and_does_not_requeue() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
    let before=map(json!({"charging_currency":"USD"}));let after=map(json!({"charging_currency":"CAD"}));
    let store=DriveStore::open_memory().unwrap();stage(&file,&store,&before,&after);let queue=store.dirty_mutables().unwrap();
    let mut current=after.clone();current.insert("unrelated".into(),json!("newer"));file_store::save(&file,&current).unwrap();
    assert_eq!(recover(&file,&legacy,&store).unwrap(),current);
    assert_eq!(store.dirty_mutables().unwrap(),queue);assert!(saved(&store).is_none());
    assert_eq!(recover(&file,&legacy,&store).unwrap(),current);assert_eq!(store.dirty_mutables().unwrap(),queue);
}

#[test]
fn conflicting_rate_values_or_a_different_path_preserve_the_journal_and_file() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
    let before=map(json!({"charging_default_rate":0.2}));let after=map(json!({"charging_default_rate":0.3}));
    let store=DriveStore::open_memory().unwrap();let expected=stage(&file,&store,&before,&after);
    file_store::save(&file,&map(json!({"charging_default_rate":0.8}))).unwrap();let bytes=std::fs::read(&file).unwrap();
    assert!(recover(&file,&legacy,&store).is_err());assert_eq!(std::fs::read(&file).unwrap(),bytes);assert_eq!(saved(&store),Some(expected.clone()));
    let other=dir.path().join("other.json");file_store::save(&other,&before).unwrap();
    assert!(recover(&other,&legacy,&store).is_err());assert_eq!(file_store::load(&other,&legacy).unwrap(),before);
    assert_eq!(saved(&store),Some(expected));
}

#[test]
fn failed_receipt_cleanup_is_recoverable_after_the_file_changed() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
    let before=map(json!({"charging_currency":"USD"}));let after=map(json!({"charging_currency":"CAD"}));
    file_store::save(&file,&before).unwrap();let store=DriveStore::open_memory().unwrap();
    store.with_locked_conn(|conn|conn.execute_batch(&format!("CREATE TRIGGER fail_cleanup BEFORE DELETE ON meta WHEN OLD.key='{RATE_PREFERENCES_JOURNAL}' BEGIN SELECT RAISE(ABORT,'synthetic cleanup failure'); END;"))).unwrap();
    assert!(publish(&file,&store,&before,&after,false).is_err());assert_eq!(file_store::load(&file,&legacy).unwrap(),after);
    let queue=store.dirty_mutables().unwrap();assert!(saved(&store).is_some());assert!(recover(&file,&legacy,&store).is_err());
    store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_cleanup")).unwrap();
    assert_eq!(recover(&file,&legacy,&store).unwrap(),after);assert_eq!(store.dirty_mutables().unwrap(),queue);assert!(saved(&store).is_none());
}

#[test]
fn staged_rate_save_survives_a_database_reopen() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("synthetic.db");let file=dir.path().join("prefs.json");let legacy=dir.path().join("legacy.json");
    let before=map(json!({"charging_default_rate":0.2}));let after=map(json!({"charging_default_rate":0.3}));
    file_store::save(&file,&before).unwrap();let store=DriveStore::open(db.to_str().unwrap()).unwrap();stage(&file,&store,&before,&after);
    let original=store.dirty_mutables().unwrap();drop(store);
    let reopened=DriveStore::open(db.to_str().unwrap()).unwrap();
    assert_eq!(recover(&file,&legacy,&reopened).unwrap(),after);assert_eq!(reopened.dirty_mutables().unwrap(),original);assert!(saved(&reopened).is_none());
}

#[test]
fn storage_rejects_unrelated_fields_before_creating_rate_intent() {
    let store=DriveStore::open_memory().unwrap();
    assert!(store.stage_rate_preferences(&json!({"private_token":"synthetic"}),&json!({}),false,"{}").is_err());
    assert!(store.dirty_mutables().unwrap().is_empty());assert!(saved(&store).is_none());
}

fn network_receipt(store:&DriveStore)->(String,String) {
    let key=format!("cloud_rate_publication_v3:{}","A".repeat(43));
    let raw="synthetic-exact-encrypted-attempt".to_string();
    store.with_locked_conn(|conn|schema::meta_set(conn,&key,&raw)).unwrap();(key,raw)
}
fn queue_rate(file:&Path,store:&DriveStore,before:Value,after:Value)->i64 {
    publish(file,store,&map(before),&map(after),false).unwrap();store.rate_sync_snapshot().unwrap().generation
}
#[test]
fn confirmation_preserves_newer_local_fields_and_retires_only_the_confirmed_prefix() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let store=DriveStore::open_memory().unwrap();
    let a=json!({"charging_default_rate":0.1,"private":true});let b=json!({"charging_default_rate":0.2,"private":true});let c=json!({"charging_default_rate":0.3,"private":true});
    let through=queue_rate(&file,&store,a,b.clone());let newer=queue_rate(&file,&store,b,c);
    let (key,raw)=network_receipt(&store);
    confirm(&file,&legacy,&store,&json!({"charging_default_rate":0.25,"charging_currency":"CAD"}),through,Some((&key,&raw))).unwrap();
    assert_eq!(file_store::load(&file,&legacy).unwrap(),map(json!({"charging_default_rate":0.3,"charging_currency":"CAD","private":true})));
    let pending=store.rate_sync_snapshot().unwrap();assert_eq!(pending.generation,newer);assert_eq!(pending.intent.edits.len(),1);assert_eq!(pending.intent.edits[0].0,newer);
    assert!(store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().is_none());assert!(saved(&store).is_none());
    confirm(&file,&legacy,&store,&json!({"charging_default_rate":0.3,"charging_currency":"CAD"}),newer,None).unwrap();
    assert!(store.dirty_mutables().unwrap().is_empty());
}
#[test]
fn interrupted_confirmation_recovers_after_file_publication_without_outgoing_echo() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let db=dir.path().join("drives.sqlite");
    let store=DriveStore::open(db.to_str().unwrap()).unwrap();
    let through=queue_rate(&file,&store,json!({}),json!({"charging_default_rate":0.2}));let (key,raw)=network_receipt(&store);
    store.with_locked_conn(|conn|conn.execute_batch(&format!("CREATE TRIGGER fail_confirm BEFORE DELETE ON meta WHEN OLD.key='{RATE_PREFERENCES_JOURNAL}' BEGIN SELECT RAISE(ABORT,'interrupted'); END;"))).unwrap();
    assert!(confirm(&file,&legacy,&store,&json!({"charging_default_rate":0.2,"charging_currency":"CAD"}),through,Some((&key,&raw))).is_err());
    assert_eq!(file_store::load(&file,&legacy).unwrap()["charging_currency"],"CAD");
    assert_eq!(store.rate_sync_snapshot().unwrap().intent.edits.len(),1);
    assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap(),Some(raw));
    assert!(saved(&store).is_some());
    drop(store);
    let store=DriveStore::open(db.to_str().unwrap()).unwrap();
    store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_confirm")).unwrap();
    recover(&file,&legacy,&store).unwrap();assert!(store.dirty_mutables().unwrap().is_empty());assert!(saved(&store).is_none());
    assert!(store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().is_none());
}
#[test]
fn confirmation_before_file_recovery_checks_the_exact_receipt_and_preserves_private_fields() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let legacy=dir.path().join("legacy");let store=DriveStore::open_memory().unwrap();
    let through=queue_rate(&file,&store,json!({}),json!({"charging_default_rate":0.2,"private":"newer"}));let (key,receipt)=network_receipt(&store);
    let desired=map(json!({"charging_default_rate":0.25,"charging_currency":"CAD"}));
    let pending=Pending {version:2,path:file.to_str().unwrap().into(),before_hash:hash(&map(json!({"charging_default_rate":0.2}))).unwrap(),
        after_hash:hash(&desired).unwrap(),rates:desired,confirmation:Some(Confirmation {through,snapshot:store.rate_sync_snapshot().unwrap(),receipt:Some((key.clone(),receipt.clone()))})};
    let raw=serde_json::to_string(&pending).unwrap();store.stage_rate_confirmation(pending.confirmation.as_ref().unwrap(),&raw).unwrap();
    store.with_locked_conn(|conn|schema::meta_set(conn,&key,"different attempt")).unwrap();
    assert!(recover(&file,&legacy,&store).is_err());assert_eq!(file_store::load(&file,&legacy).unwrap()["charging_default_rate"],0.2);
    store.with_locked_conn(|conn|schema::meta_set(conn,&key,&receipt)).unwrap();
    let current=recover(&file,&legacy,&store).unwrap();assert_eq!(current["private"],"newer");assert_eq!(current["charging_default_rate"],0.25);
    assert!(store.dirty_mutables().unwrap().is_empty());assert!(saved(&store).is_none());
}
#[test]
fn changed_queue_refuses_confirmation_before_any_file_or_receipt_changes() {
    let dir=tempfile::tempdir().unwrap();let file=dir.path().join("prefs");let store=DriveStore::open_memory().unwrap();
    let through=queue_rate(&file,&store,json!({}),json!({"charging_default_rate":0.2}));let (key,raw)=network_receipt(&store);
    let confirmation=Confirmation {through,snapshot:store.rate_sync_snapshot().unwrap(),receipt:Some((key.clone(),raw.clone()))};
    queue_rate(&file,&store,json!({"charging_default_rate":0.2}),json!({"charging_default_rate":0.3}));
    assert!(store.stage_rate_confirmation(&confirmation,"journal").is_err());assert!(saved(&store).is_none());
    assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap(),Some(raw));
    assert_eq!(store.rate_sync_snapshot().unwrap().intent.edits.len(),2);
}

//! One checked rate snapshot and one journaled save for the native editor.
use super::*;
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    expected: Map<String, Value>,
    document: Map<String, Value>,
}
impl SaveRequest {
    fn valid(&self) -> bool {
        self.expected.keys().chain(self.document.keys())
            .all(|key| RATE_CONFIG_KEYS.contains(&key.as_str()))
    }
}

pub async fn get_rates(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let store = state.drives.store.clone();
    match tokio::task::spawn_blocking(move || load_prefs_checked(&store).map(|prefs| rate_fields(&prefs))).await {
        Ok(Ok(document)) => (StatusCode::OK, Json(serde_json::json!({"document": document}))),
        _ => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Rates could not be read. Try again."),
    }
}

fn save_at(primary: &std::path::Path, legacy: &std::path::Path,
    store: &sentryusb_drives::DriveStore, request: &SaveRequest,
) -> anyhow::Result<Option<Map<String, Value>>> {
    anyhow::ensure!(request.valid(), "unexpected rate preference key");
    let _guard = PREFS_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = journal::recover(primary, legacy, store)?;
    let current = rate_fields(&before);
    if current == request.document { return Ok(Some(current)); }
    if current != request.expected { return Ok(None); }
    let mut after = before.clone();
    for key in RATE_CONFIG_KEYS {
        match request.document.get(*key) {
            Some(value) => { after.insert((*key).into(), value.clone()); }
            None => { after.remove(*key); }
        }
    }
    journal::publish(primary, store, &before, &after, false)?;
    Ok(Some(rate_fields(&after)))
}

pub async fn save_rates(State(state): State<AppState>, body: String) -> (StatusCode, Json<Value>) {
    let request = match serde_json::from_str::<SaveRequest>(&body) {
        Ok(request) if request.valid() => request,
        _ => return crate::json_error(StatusCode::BAD_REQUEST, "Invalid rate settings."),
    };
    let store = state.drives.store.clone();
    let saved = tokio::task::spawn_blocking(move || save_at(
        std::path::Path::new(&prefs_file()), std::path::Path::new(&legacy_prefs_file()), &store, &request,
    )).await;
    // An uncertain file save may already have durably queued the edit.
    state.cloud.uploader.nudge();
    match saved {
        Ok(Ok(Some(document))) => (StatusCode::OK, Json(serde_json::json!({"document": document}))),
        Ok(Ok(None)) => crate::json_error(StatusCode::CONFLICT, "Rates changed elsewhere. Close and reopen the editor before saving."),
        _ => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Rate save could not be confirmed. Close and reopen the editor to check."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn request(expected: Value, document: Value) -> SaveRequest {
        SaveRequest { expected: expected.as_object().unwrap().clone(), document: document.as_object().unwrap().clone() }
    }
    #[test]
    fn all_rate_fields_commit_as_one_intent_and_unrelated_preferences_survive() {
        let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("prefs"); let legacy = dir.path().join("legacy");
        let store = sentryusb_drives::DriveStore::open_memory().unwrap();
        file_store::save(&file, json!({"private":"retained","charging_currency":"CAD"}).as_object().unwrap()).unwrap();
        let edit = request(json!({"charging_currency":"CAD"}), json!({"charging_currency":"USD","charging_default_rate":0.2,"charging_tag_rates":{"Home":0.1}}));
        assert_eq!(save_at(&file,&legacy,&store,&edit).unwrap(),Some(edit.document.clone()));
        assert_eq!(file_store::load(&file,&legacy).unwrap()["private"],"retained");
        let intent = store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"rate","")).unwrap();
        assert_eq!(intent.edits.len(),1);
        assert_eq!(save_at(&file,&legacy,&store,&edit).unwrap(),Some(edit.document.clone()));
        let after = store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"rate","")).unwrap();
        assert_eq!(after.edits.len(),1);
        let stale = request(edit.expected.clone().into(),json!({"charging_currency":"GBP"}));
        assert!(save_at(&file,&legacy,&store,&stale).unwrap().is_none());
        assert_eq!(rate_fields(&file_store::load(&file,&legacy).unwrap()),edit.document);
    }
    #[test]
    fn malformed_file_and_unrelated_input_cannot_replace_preferences() {
        let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("prefs"); let legacy = dir.path().join("legacy");
        let store = sentryusb_drives::DriveStore::open_memory().unwrap();
        std::fs::write(&file,b"broken").unwrap();
        assert!(save_at(&file,&legacy,&store,&request(json!({}),json!({"charging_currency":"CAD"}))).is_err());
        assert!(save_at(&file,&legacy,&store,&request(json!({}),json!({"other":true}))).is_err());
        assert_eq!(std::fs::read(&file).unwrap(),b"broken");
        assert!(store.dirty_mutables().unwrap().is_empty());
        assert!(serde_json::from_value::<SaveRequest>(json!({"expected":{},"document":{},"extra":true})).is_err());
    }
    #[test]
    fn journal_failure_leaves_the_entire_prior_document_intact() {
        let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("prefs"); let legacy = dir.path().join("legacy");
        let store = sentryusb_drives::DriveStore::open_memory().unwrap();
        let before = json!({"charging_currency":"CAD","charging_default_rate":0.1});
        file_store::save(&file,before.as_object().unwrap()).unwrap();
        store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_edit BEFORE INSERT ON mutable_dirty BEGIN SELECT RAISE(ABORT,'synthetic'); END;")).unwrap();
        assert!(save_at(&file,&legacy,&store,&request(before.clone(),json!({"charging_currency":"USD","charging_default_rate":0.2}))).is_err());
        assert_eq!(file_store::load(&file,&legacy).unwrap(),before.as_object().unwrap().clone());
        assert!(store.dirty_mutables().unwrap().is_empty());
    }
}

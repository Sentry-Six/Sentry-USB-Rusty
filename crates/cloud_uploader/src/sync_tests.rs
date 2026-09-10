use super::*;
use crate::encrypt::CostOverride;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use sentryusb_cloud_crypto::aead;
use sentryusb_drives::DriveStore;
use sentryusb_ws::Hub;
use tokio::sync::Notify;

const PI_KEY: [u8; 32] = [7; 32];
const CHARGE_KEY: [u8; 32] = [9; 32];

fn state() -> Arc<CloudStateInner> {
    let store = Arc::new(DriveStore::open_memory().unwrap());
    Arc::new(CloudStateInner::new(store, Hub::new(), Arc::new(Notify::new()),
        "http://127.0.0.1:1".into(), String::new(), None))
}

fn charge_page(state: &CloudStateInner) -> ChangesResponse {
    let id = "ab".repeat(32);
    let wrapped = B64.encode(aead::seal(&aead::Key::from_bytes(&PI_KEY).unwrap(),
        &aad::charge_key("user", "pi", &id), &CHARGE_KEY).unwrap());
    state.store.charge_upload_mark(100, &id, &wrapped, 1).unwrap();
    state.store.set_charge_tags_from_sync(100, &["Before".into()]).unwrap();
    state.store.set_charge_cost_from_sync(100, Some((4.0, "CAD".into()))).unwrap();
    let mutable = ChargeMutable { at_home:None, tags: vec!["After".into(), "Home".into()],
        cost_override: Some(CostOverride { amount: 8.0, currency: "CAD".into() }) };
    ChangesResponse { routes: vec![], rate_config: None,
        charges: vec![ChargeChange { charge_id: id.clone(), wrapped_charge_key: wrapped,
            mutable_ciphertext: Some(encrypt::seal_json_b64(&CHARGE_KEY,
                &aad::charge_mutable("user", "pi", &id), &mutable).unwrap()), updated_at_ms: 15 }] }
}

#[test]
fn failed_charge_decryption_preserves_local_state_then_retries() {
    let state = state();
    let mut page = charge_page(&state);
    let correct = page.charges[0].mutable_ciphertext.clone();
    page.charges[0].mutable_ciphertext = Some("invalid".into());
    assert!(apply_change_page(&state, "user", "pi", &PI_KEY, &page).is_err());
    assert_eq!(state.store.get_charge_tags(100).unwrap(), vec!["Before"]);
    page.charges[0].mutable_ciphertext = correct;
    apply_change_page(&state, "user", "pi", &PI_KEY, &page).unwrap();
    assert_eq!(state.store.get_charge_tags(100).unwrap(), vec!["After"]);
    assert_eq!(state.store.get_charge_cost(100).unwrap(), Some((8.0, "CAD".into())));
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn failed_charge_save_rolls_back_tags_and_cost() {
    let state = state();
    let page = charge_page(&state);
    state.store.with_locked_conn(|conn| conn.execute_batch(
        "CREATE TRIGGER fail_cost BEFORE INSERT ON charge_costs BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;"
    )).unwrap();
    assert!(apply_change_page(&state, "user", "pi", &PI_KEY, &page).is_err());
    assert_eq!(state.store.get_charge_tags(100).unwrap(), vec!["Before"]);
    assert_eq!(state.store.get_charge_cost(100).unwrap(), Some((4.0, "CAD".into())));
    state.store.with_locked_conn(|conn| conn.execute_batch("DROP TRIGGER fail_cost")).unwrap();
    apply_change_page(&state, "user", "pi", &PI_KEY, &page).unwrap();
    assert_eq!(state.store.get_charge_tags(100).unwrap(), vec!["After"]);
}

#[test]
fn failed_local_lookup_does_not_mean_deleted_session() {
    let state = state();
    let page = charge_page(&state);
    state.store.with_locked_conn(|conn| conn.execute_batch("ALTER TABLE charge_uploads RENAME TO hidden_charge_uploads")).unwrap();
    assert!(apply_change_page(&state, "user", "pi", &PI_KEY, &page).is_err());
}

#[test]
fn missing_local_session_is_skipped_without_recreating_it() {
    let state = state();
    let page = charge_page(&state);
    state.store.with_locked_conn(|conn| conn.execute_batch("DELETE FROM charge_uploads; DELETE FROM charge_tags; DELETE FROM charge_costs;")).unwrap();
    apply_change_page(&state, "user", "pi", &PI_KEY, &page).unwrap();
    assert!(state.store.get_charge_tags(100).unwrap().is_empty());
    assert_eq!(state.store.get_charge_cost(100).unwrap(), None);
}

#[test]
fn pending_newer_local_edit_survives_remote_page() {
    let state = state();
    let page = charge_page(&state);
    state.store.set_charge_tags(100, &["Local".into()]).unwrap();
    apply_change_page(&state, "user", "pi", &PI_KEY, &page).unwrap();
    assert_eq!(state.store.get_charge_tags(100).unwrap(), vec!["Local"]);
    assert_eq!(state.store.get_charge_cost(100).unwrap(), Some((4.0, "CAD".into())));
    assert_eq!(state.store.dirty_mutables().unwrap().len(), 1);
}

struct BrokenRates;
impl crate::state::RateConfigAccess for BrokenRates {
    fn load_doc(&self,_store:&sentryusb_drives::DriveStore) -> Result<serde_json::Value> { Ok(serde_json::json!({})) }
    fn store_doc(&self, _: &serde_json::Value, _: &DriveStore, _: i64) -> Result<()> { anyhow::bail!("synthetic rate write failure") }
}

#[test]
fn failed_rate_save_reports_page_failure() {
    let mut state = state();
    Arc::get_mut(&mut state).unwrap().rate_config = Some(Arc::new(BrokenRates));
    let page = ChangesResponse { routes: vec![], charges: vec![],
        rate_config: Some(RateConfigChange { updated_at_ms: 15, ciphertext: Some(encrypt::seal_json_b64(
            &PI_KEY, &aad::rate_config("user", "pi"), &serde_json::json!({"charging_default_rate": 0.1})).unwrap()) }) };
    assert!(apply_change_page(&state, "user", "pi", &PI_KEY, &page).is_err());
}

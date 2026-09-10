use super::*;
use super::revision::tests::fixture;
use serde_json::json;
const PI_KEY: [u8;32]=[7;32];

struct FailedRateRead;
impl crate::state::RateConfigAccess for FailedRateRead {
    fn load_doc(&self,_store:&sentryusb_drives::DriveStore) -> Result<serde_json::Value> { anyhow::bail!("synthetic rate read failure") }
    fn store_doc(&self, _: &serde_json::Value, _: &sentryusb_drives::DriveStore, _: i64) -> Result<()> { Ok(()) }
}

#[tokio::test]
async fn failed_rate_read_does_not_publish_empty_configuration() {
    let (mut state, creds) = fixture();
    Arc::get_mut(&mut state).unwrap().rate_config = Some(Arc::new(FailedRateRead));
    state.store.mark_rate_config_dirty().unwrap();
    let client = CloudClient::new("http://127.0.0.1:1").with_bearer(&[3;32]);
    let error = conditional_rate::push(&state, &client, &creds, &PI_KEY).await.unwrap_err();
    assert!(format!("{error:#}").contains("synthetic rate read failure"));
    assert_eq!(state.store.dirty_mutables().unwrap().len(), 1);
}

struct ChangedRateRead(Arc<sentryusb_drives::DriveStore>);
impl crate::state::RateConfigAccess for ChangedRateRead {
    fn load_doc(&self,_store:&sentryusb_drives::DriveStore) -> Result<serde_json::Value> {
        self.0.mark_rate_config_dirty()?;
        Ok(json!({"charging_default_rate":0.2}))
    }
    fn store_doc(&self, _: &serde_json::Value, _: &sentryusb_drives::DriveStore, _: i64) -> Result<()> { Ok(()) }
}

#[tokio::test]
async fn newer_rate_read_is_not_published_with_an_older_queue_stamp() {
    let (mut state, creds) = fixture(); let store = state.store.clone();
    Arc::get_mut(&mut state).unwrap().rate_config = Some(Arc::new(ChangedRateRead(store)));
    state.store.mark_rate_config_dirty().unwrap(); let before = state.store.dirty_mutables().unwrap()[0].2;
    let client = CloudClient::new("http://127.0.0.1:1").with_bearer(&[3;32]);
    conditional_rate::push(&state, &client, &creds, &PI_KEY).await.unwrap();
    assert!(state.store.dirty_mutables().unwrap()[0].2 > before);
}

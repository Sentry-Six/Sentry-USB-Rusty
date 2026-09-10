use super::*;
use sentryusb_cloud_crypto::credentials::LongTermX25519OnDisk;
fn fixture()->(Arc<CloudStateInner>,CloudCredentialsV1) {
    let creds=CloudCredentialsV1 {version:1,user_id:"owner".into(),pi_id:"pi".into(),pi_auth_token:"synthetic-token".into(),
        wrapped_pi_key_local:"synthetic-key".into(),long_term_x25519:LongTermX25519OnDisk {public_key:"synthetic-public".into(),wrapped_private_key:"synthetic-wrapped".into()},
        cloud_base_url:"https://synthetic.invalid".into(),paired_at:"2026-01-01T00:00:00Z".parse().unwrap(),dek_rotation_generation:0};
    let mut state=CloudStateInner::new(Arc::new(DriveStore::open_memory().unwrap()),Hub::new(),Arc::new(Notify::new()),creds.cloud_base_url.clone(),String::new(),None);
    *state.creds.get_mut()=Some(creds.clone());(Arc::new(state),creds)
}

#[tokio::test]
async fn pending_edits_and_failed_stages_are_independent_of_upload_success() {
    let (state,creds)=fixture();
    state.store.set_drive_tags("one",&["Work".into()]).unwrap();state.store.set_drive_tags("one",&["Personal".into()]).unwrap();
    state.store.set_drive_tags("two",&["Work".into()]).unwrap();state.store.set_charge_tags(1,&["Work".into()]).unwrap();
    state.store.mark_rate_config_dirty().unwrap();
    let ticket=state.begin_sync(&creds).await.unwrap();assert!(state.snapshot_status().await.mutable_sync.running);
    state.finish_sync(&creds,ticket,vec![SyncStage::DriveTags,SyncStage::Incoming]).await.unwrap();
    *state.last_upload_error.lock().await=None;
    let status=state.snapshot_status().await.mutable_sync;
    assert!(!status.running);assert!(status.last_attempt_at.is_some());
    assert_eq!(status.failed_stages,vec![SyncStage::DriveTags,SyncStage::Incoming]);
    assert_eq!(status.pending_edits,Some(PendingEdits {drive_tags:2,charging:1,rates:1}));
    let encoded=serde_json::to_string(&status).unwrap();assert!(!encoded.contains("synthetic"));assert!(!encoded.contains("Work"));
    let ticket=state.begin_sync(&creds).await.unwrap();state.finish_sync(&creds,ticket,vec![]).await.unwrap();
    let status=state.snapshot_status().await.mutable_sync;assert!(status.failed_stages.is_empty());
    assert_eq!(status.pending_edits.unwrap().drive_tags,2,"reporting a completed pass cannot retire edits");
}

#[tokio::test]
async fn an_unreadable_queue_is_unavailable_rather_than_zero_pending_edits() {
    let (state,_)=fixture();
    state.store.with_locked_conn(|conn|conn.execute_batch("ALTER TABLE mutable_dirty RENAME TO synthetic_hidden_queue")).unwrap();
    assert!(state.snapshot_status().await.mutable_sync.pending_edits.is_none());
}

#[tokio::test]
async fn late_status_cannot_complete_a_new_run_or_leak_across_pairings() {
    let (state,creds)=fixture();let first=state.begin_sync(&creds).await.unwrap();let second=state.begin_sync(&creds).await.unwrap();
    assert!(!state.finish_sync(&creds,first,vec![]).await.unwrap());assert!(state.snapshot_status().await.mutable_sync.running);
    assert!(state.finish_sync(&creds,second,vec![SyncStage::Charging]).await.unwrap());
    let mut next=creds.clone();next.pi_id="next-pi".into();*state.creds.lock().await=Some(next.clone());
    assert!(state.finish_sync(&creds,second,vec![SyncStage::Credentials]).await.is_err());
    let status=state.snapshot_status().await.mutable_sync;assert!(!status.running);assert!(status.failed_stages.is_empty());assert!(status.last_attempt_at.is_none());
    state.begin_sync(&next).await.unwrap();let status=state.snapshot_status().await.mutable_sync;
    assert!(status.running);assert!(status.failed_stages.is_empty());assert!(status.last_attempt_at.is_none());
}

#[tokio::test]
async fn credentials_failure_is_reported_by_the_actual_sync_entrypoint() {
    let (state,_)=fixture();
    assert!(crate::sync::run_once(state.clone()).await.is_err());
    let status=state.snapshot_status().await.mutable_sync;
    assert!(!status.running);assert_eq!(status.failed_stages,vec![SyncStage::Credentials]);assert!(status.last_attempt_at.is_some());
}

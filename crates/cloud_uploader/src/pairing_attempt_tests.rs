use super::*;
use crate::state::{PairingState,CloudStateInner};
use sentryusb_cloud_crypto::credentials::{CloudCredentialsV1,LongTermX25519OnDisk};
use sentryusb_drives::DriveStore;
use sentryusb_ws::Hub;
use tokio::sync::Notify;
fn fixture()->(Arc<CloudStateInner>,CloudCredentialsV1,std::path::PathBuf) {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let suffix:String=nonce.iter().map(|byte|format!("{byte:02x}")).collect();
    let directory=std::env::temp_dir().join(format!("sentry-pair-attempt-{suffix}"));std::fs::create_dir(&directory).unwrap();
    let path=directory.join("synthetic-credentials.json");
    let state=Arc::new(CloudStateInner::new(Arc::new(DriveStore::open_memory().unwrap()),Hub::new(),Arc::new(Notify::new()),
        "http://127.0.0.1:1".into(),path.to_str().unwrap().into(),None));
    let creds=CloudCredentialsV1 {version:1,user_id:"synthetic-owner".into(),pi_id:"synthetic-pi".into(),pi_auth_token:"synthetic-token".into(),
        wrapped_pi_key_local:"synthetic-wrapped-key".into(),long_term_x25519:LongTermX25519OnDisk {public_key:"synthetic-public".into(),wrapped_private_key:"synthetic-private-wrap".into()},
        cloud_base_url:"https://synthetic.invalid".into(),paired_at:"2026-01-01T00:00:00Z".parse().unwrap(),dek_rotation_generation:0};
    (state,creds,directory)
}

#[tokio::test]
async fn duplicate_begin_and_late_cancelled_progress_cannot_replace_a_new_attempt() {
    let (state,creds,directory)=fixture();let first=state.begin_pairing().await.unwrap();
    assert!(state.begin_pairing().await.is_err());
    state.cancel_pairing().await;
    let next=state.begin_pairing().await.unwrap();
    assert!(state.pairing_progress(&first,PairingState::Polling).await.is_err());
    assert!(state.complete_pairing(&first,creds.clone()).await.is_err());
    state.fail_pairing(&first).await;
    assert_eq!(state.pairing.lock().await.state,PairingState::Handshaking);
    assert!(state.creds.lock().await.is_none());assert!(!std::path::Path::new(&state.credentials_path).exists());
    state.complete_pairing(&next,creds.clone()).await.unwrap();
    assert_eq!(state.creds.lock().await.as_ref(),Some(&creds));
    assert_eq!(state.pairing.lock().await.state,PairingState::Complete);
    assert!(state.pairing_cancel.lock().await.is_none());
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unpair_cancels_a_pending_attempt_before_it_can_publish() {
    let (state,creds,directory)=fixture();let attempt=state.begin_pairing().await.unwrap();
    state.unpair().await.unwrap();
    assert!(state.complete_pairing(&attempt,creds).await.is_err());
    assert!(state.creds.lock().await.is_none());assert!(!std::path::Path::new(&state.credentials_path).exists());
    assert_eq!(state.pairing.lock().await.state,PairingState::Idle);
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn an_existing_new_credential_file_cannot_be_overwritten_by_old_pairing() {
    let (state,creds,directory)=fixture();let attempt=state.begin_pairing().await.unwrap();
    let mut newer=creds.clone();newer.pi_id="newer-pi".into();state.set_credentials(newer.clone()).await.unwrap();
    let original=std::fs::read(&state.credentials_path).unwrap();
    assert!(state.complete_pairing(&attempt,creds).await.is_err());
    assert_eq!(state.creds.lock().await.as_ref(),Some(&newer));assert_eq!(std::fs::read(&state.credentials_path).unwrap(),original);
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn failed_credential_removal_preserves_the_pairing_and_target() {
    let (state,creds,directory)=fixture();
    std::fs::create_dir(&state.credentials_path).unwrap();
    let keep=std::path::Path::new(&state.credentials_path).join("keep");std::fs::write(&keep,b"untouched").unwrap();
    *state.creds.lock().await=Some(creds.clone());
    assert!(state.unpair().await.is_err());
    assert_eq!(state.creds.lock().await.as_ref(),Some(&creds));assert_eq!(std::fs::read(keep).unwrap(),b"untouched");
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn real_handshake_failure_finishes_the_owned_attempt_instead_of_leaving_a_spinner() {
    use tokio::{net::TcpListener,io::{AsyncReadExt,AsyncWriteExt}};
    let (mut state,_,directory)=fixture();let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut state).unwrap().cloud_base_url=format!("http://{}",listener.local_addr().unwrap());
    let server=tokio::spawn(async move {
        let (mut socket,_)=listener.accept().await.unwrap();let mut data=[0u8;8192];let _=socket.read(&mut data).await.unwrap();
        socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
    });
    assert!(run(state.clone(),"123456".into()).await.is_err());server.await.unwrap();
    assert_eq!(state.pairing.lock().await.state,PairingState::Error);assert!(state.pairing_cancel.lock().await.is_none());
    assert!(state.creds.lock().await.is_none());
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cancel_during_handshake_stops_the_request_and_leaves_a_new_attempt_intact() {
    use tokio::{net::TcpListener,io::AsyncReadExt,sync::oneshot};
    let (mut state,_,directory)=fixture();let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut state).unwrap().cloud_base_url=format!("http://{}",listener.local_addr().unwrap());
    let (seen,received)=oneshot::channel();let (release,hold)=oneshot::channel::<()>();
    let server=tokio::spawn(async move {
        let (mut socket,_)=listener.accept().await.unwrap();let mut data=[0u8;8192];let _=socket.read(&mut data).await.unwrap();
        seen.send(()).unwrap();let _=hold.await;
    });
    let task=tokio::spawn(run(state.clone(),"123456".into()));received.await.unwrap();
    state.cancel_pairing().await;let next=state.begin_pairing().await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap().is_err());
    assert_eq!(state.pairing.lock().await.state,PairingState::Handshaking);
    assert!(Arc::ptr_eq(state.pairing_cancel.lock().await.as_ref().unwrap(),&next));
    release.send(()).unwrap();server.await.unwrap();
    state.cancel_pairing().await;drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn background_start_reserves_the_attempt_before_returning_accepted() {
    use tokio::{net::TcpListener,io::AsyncReadExt,sync::oneshot};
    let (mut state,_,directory)=fixture();let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut state).unwrap().cloud_base_url=format!("http://{}",listener.local_addr().unwrap());
    let (seen,received)=oneshot::channel();let (release,hold)=oneshot::channel::<()>();
    let server=tokio::spawn(async move {
        let (mut socket,_)=listener.accept().await.unwrap();let mut data=[0u8;8192];let _=socket.read(&mut data).await.unwrap();
        seen.send(()).unwrap();let _=hold.await;
    });
    start(state.clone(),"123456".into()).await.unwrap();
    assert_eq!(state.pairing.lock().await.state,PairingState::Handshaking);
    assert!(start(state.clone(),"654321".into()).await.is_err());
    received.await.unwrap();state.cancel_pairing().await;release.send(()).unwrap();server.await.unwrap();
    assert!(state.creds.lock().await.is_none());
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

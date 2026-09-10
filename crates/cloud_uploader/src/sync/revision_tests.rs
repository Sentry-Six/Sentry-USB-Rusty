use super::*;
use std::future::Future;
use sentryusb_cloud_crypto::{aad, aead, credentials::LongTermX25519OnDisk};
use sentryusb_drives::DriveStore;
use sentryusb_ws::Hub;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, sync::Notify};
use serde_json::{Value, json};

const PI_KEY: [u8; 32] = [7; 32];
const CONTENT_KEY: [u8; 32] = [9; 32];

pub(in crate::sync) fn fixture() -> (Arc<CloudStateInner>, CloudCredentialsV1) {
    let creds = CloudCredentialsV1 { version: 1, user_id: "owner".into(), pi_id: "pi".into(),
        pi_auth_token: "synthetic-sealed-token".into(), wrapped_pi_key_local: "synthetic-sealed-key".into(),
        long_term_x25519: LongTermX25519OnDisk { public_key: "synthetic-public".into(), wrapped_private_key: "synthetic-wrapped".into() },
        cloud_base_url: "https://synthetic.invalid".into(), paired_at: "2026-01-01T00:00:00Z".parse().unwrap(), dek_rotation_generation: 0 };
    let mut state = CloudStateInner::new(Arc::new(DriveStore::open_memory().unwrap()), Hub::new(),
        Arc::new(Notify::new()), creds.cloud_base_url.clone(), String::new(), None);
    *state.creds.get_mut() = Some(creds.clone());
    (Arc::new(state), creds)
}

pub(in crate::sync) fn charge(state: &CloudStateInner, id: i64, rev: &str) -> Value {
    let charge_id = format!("{id:064x}");
    let wrapped = B64.encode(aead::seal(&aead::Key::from_bytes(&PI_KEY).unwrap(),
        &aad::charge_key("owner", "pi", &charge_id), &CONTENT_KEY).unwrap());
    state.store.charge_upload_mark(id, &charge_id, &wrapped, 1).unwrap();
    state.store.set_charge_tags_from_sync(id, &["Before".into()]).unwrap();
    let mutable = crate::encrypt::ChargeMutable { at_home:None, tags: vec!["After".into()], cost_override: None };
    let ct = crate::encrypt::seal_json_b64(&CONTENT_KEY, &aad::charge_mutable("owner", "pi", &charge_id), &mutable).unwrap();
    json!({"kind":"charge", "id":charge_id, "revision":rev, "updatedAtMs":15, "ciphertext":ct, "wrappedKey":wrapped})
}

fn page(items: Vec<Value>, revision: &str, next: Option<&str>) -> Value {
    json!({"ok":true,"items":items,"nextCursor":next,"sync":{"version":2,"revision":revision}})
}

fn checkpoint(state: &CloudStateInner) -> Option<String> {
    state.store.with_locked_conn(|conn| schema::meta_get(conn, CHECKPOINT_KEY)).unwrap()
        .map(|raw| serde_json::from_str::<Checkpoint>(&raw).unwrap().revision)
}

fn seed(state: &CloudStateInner, creds: &CloudCredentialsV1, revision: &str) {
    let raw = serde_json::to_string(&Checkpoint { version: 2, binding: binding(creds).unwrap(), revision: revision.into() }).unwrap();
    state.store.with_locked_conn(|conn| schema::meta_set(conn, CHECKPOINT_KEY, &raw)).unwrap();
}

// Real HTTP/reqwest transport. Credentials remain synthetic and only request
// paths are retained; no bearer value is logged or asserted in failure output.
pub(in crate::sync) async fn server<F, Fut>(replies: Vec<(u16, Value)>, mut before_send: F)
    -> (CloudClient, Arc<std::sync::Mutex<Vec<String>>>, tokio::task::JoinHandle<()>)
where F: FnMut(usize) -> Fut + Send + 'static, Fut: Future<Output = ()> + Send {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved_paths = paths.clone();
    let task = tokio::spawn(async move {
        for (index, (status, body)) in replies.into_iter().enumerate() {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).await.unwrap();
                assert!(count > 0 && request.len() < 8192);
                request.extend_from_slice(&bytes[..count]);
            }
            let path = std::str::from_utf8(&request).unwrap().lines().next().unwrap().split_whitespace().nth(1).unwrap().to_string();
            saved_paths.lock().unwrap().push(path);
            let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let body_len = std::str::from_utf8(&request[..header_end]).unwrap().lines()
                .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                .unwrap_or(0);
            while request.len() < header_end + body_len {
                let mut bytes = [0; 4096];
                let count = socket.read(&mut bytes).await.unwrap();
                assert!(count > 0 && request.len() < 1024 * 1024);
                request.extend_from_slice(&bytes[..count]);
            }
            before_send(index).await;
            let body = serde_json::to_vec(&body).unwrap();
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (CloudClient::new(format!("http://{address}")).with_bearer(&[3; 32]), paths, task)
}

#[tokio::test]
async fn cold_reader_ignores_legacy_timestamp_and_applies_revision_zero() {
    let (state, creds) = fixture();
    state.store.with_locked_conn(|conn| schema::meta_set(conn, "cloud_sync_cursor_ms", "99999999")).unwrap();
    let item = charge(&state, 1, "0");
    let (client, paths, task) = server(vec![(200, page(vec![item], "0", None))], |_| async {}).await;
    pull_changes(&state, &client, &creds, &PI_KEY).await.unwrap();
    task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("0"));
    assert_eq!(state.store.get_charge_tags(1).unwrap(), vec!["After"]);
    assert_eq!(paths.lock().unwrap().as_slice(), &["/api/pi/sync/changes/v2?limit=200"]);
}

#[tokio::test]
async fn interrupted_walk_resumes_after_the_last_applied_page_and_preserves_new_local_edit() {
    let (state, creds) = fixture(); seed(&state, &creds, "10");
    let first = charge(&state, 1, "11"); let second = charge(&state, 2, "12");
    let mut broken = second.clone(); broken.as_object_mut().unwrap().remove("ciphertext");
    let (client, _, task) = server(vec![(200, page(vec![first.clone()], "20", Some("next_page"))),
        (200, page(vec![broken], "20", None))], |_| async {}).await;
    assert!(pull_changes(&state, &client, &creds, &PI_KEY).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("10"));
    assert_eq!(state.store.get_charge_tags(1).unwrap(), vec!["After"]);
    assert_eq!(state.store.get_charge_tags(2).unwrap(), vec!["Before"]);
    state.store.set_charge_tags(1, &["New local edit".into()]).unwrap();
    let (client, paths, task) = server(vec![(200, page(vec![second], "20", None))], |_| async {}).await;
    pull_changes(&state, &client, &creds, &PI_KEY).await.unwrap(); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("20"));
    assert_eq!(state.store.get_charge_tags(1).unwrap(), vec!["New local edit"]);
    assert_eq!(state.store.get_charge_tags(2).unwrap(), vec!["After"]);
    assert!(paths.lock().unwrap()[0].contains("since=10"));
    assert!(paths.lock().unwrap()[0].contains("cursor=next_page"));
    assert_eq!(paths.lock().unwrap().len(),1);
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,WALK_KEY)).unwrap().is_none());
}

#[tokio::test]
async fn re_pair_during_request_rejects_late_application_and_checkpoint() {
    let (state, creds) = fixture(); seed(&state, &creds, "10");
    let first = charge(&state, 1, "11"); let second = charge(&state, 2, "12");
    let changed = state.clone();
    let (client, _, task) = server(vec![(200, page(vec![first], "20", Some("next_page"))),
        (200, page(vec![second], "20", None))], move |index| {
        let changed = changed.clone(); async move { if index == 1 {
            changed.creds.lock().await.as_mut().unwrap().pi_auth_token = "replacement-token".into();
        } }
    }).await;
    assert!(pull_changes(&state, &client, &creds, &PI_KEY).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("10"));
    assert_eq!(state.store.get_charge_tags(2).unwrap(), vec!["Before"]);
}

#[tokio::test]
async fn authenticated_reset_restarts_full_walk_without_clearing_local_data() {
    let (state, creds) = fixture(); seed(&state, &creds, "99");
    let item = charge(&state, 1, "0");
    let (client, paths, task) = server(vec![(409, json!({"error":"sync_reset_required"})),
        (200, page(vec![item], "3", None))], |_| async {}).await;
    pull_changes(&state, &client, &creds, &PI_KEY).await.unwrap(); task.await.unwrap();
    assert!(paths.lock().unwrap()[0].contains("since=99"));
    assert!(!paths.lock().unwrap()[1].contains("since="));
    assert_eq!(checkpoint(&state).as_deref(), Some("3"));
    assert_eq!(state.store.get_charge_tags(1).unwrap(), vec!["After"]);
}

#[tokio::test]
async fn missing_endpoint_does_not_fall_back_to_timestamp_feed() {
    let (state, creds) = fixture(); seed(&state, &creds, "10");
    let (client, paths, task) = server(vec![(404, json!({"error":"not_found"}))], |_| async {}).await;
    assert!(pull_changes(&state, &client, &creds, &PI_KEY).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("10"));
    assert_eq!(paths.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn wrong_decryption_key_retains_the_record_before_advancing_the_walk() {
    let (state, creds) = fixture(); let item = charge(&state, 1, "0");
    let (client, _, task) = server(vec![(200, page(vec![item], "3", None))], |_| async {}).await;
    assert!(pull_changes(&state, &client, &creds, &[5; 32]).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("3"));
    assert_eq!(retry_count(&state,&creds),1);
    assert_eq!(state.store.get_charge_tags(1).unwrap(), vec!["Before"]);
}

#[test]
fn rejects_missing_nullable_fields_and_nonadvancing_or_foreign_pages() {
    let (state, _) = fixture(); let item = charge(&state, 1, "11");
    let mut missing_cursor = page(vec![item.clone()], "20", None);
    missing_cursor.as_object_mut().unwrap().remove("nextCursor");
    assert!(serde_json::from_value::<Page>(missing_cursor).is_err());
    for mut invalid in [page(vec![item.clone(), item.clone()], "20", None),
        page(vec![], "20", Some("next")), page(vec![item.clone()], "09", None)] {
        let parsed: Page = serde_json::from_value(invalid.take()).unwrap();
        assert!(Walk::new(Some("10".into())).validate(&parsed, "pi").is_err());
    }
    let mut walk = Walk::new(Some("10".into()));
    walk.validate(&serde_json::from_value(page(vec![item.clone()], "20", Some("next"))).unwrap(), "pi").unwrap();
    assert!(walk.validate(&serde_json::from_value(page(vec![item], "21", None)).unwrap(), "pi").is_err());
    assert!(revision("9223372036854775808").is_err());
}

#[test]
fn cloud_wire_vector_decrypts_with_rust_aad_and_crypto() {
    let vector: Value = serde_json::from_str(include_str!("../../test-vectors/pi-mutable-v2.json")).unwrap();
    let page: Page = serde_json::from_value(vector["wire"].clone()).unwrap();
    let user = vector["userId"].as_str().unwrap(); let pi = vector["piId"].as_str().unwrap();
    Walk::new(None).validate(&page, pi).unwrap();
    let key: [u8; 32] = B64.decode(vector["piKeyB64"].as_str().unwrap()).unwrap().try_into().unwrap();
    for item in page.items {
        let (key, aad, expected) = match item.kind {
            Kind::Charge => (crate::encrypt::unwrap_content_key(&key, item.wrapped_key.as_deref().unwrap(),
                &aad::charge_key(user, pi, &item.id)).unwrap(), aad::charge_mutable(user, pi, &item.id), "charge"),
            Kind::Route => (crate::encrypt::unwrap_content_key(&key, item.wrapped_key.as_deref().unwrap(),
                &aad::route_key(user, pi, &item.id)).unwrap(), aad::route_tags(user, pi, &item.id), "route"),
            Kind::RateConfig => (key, aad::rate_config(user, pi), "rate"),
        };
        let plaintext: Value = crate::encrypt::open_json_b64(&key, &aad, item.ciphertext.as_deref().unwrap()).unwrap();
        assert_eq!(plaintext, vector["expected"][expected]);
    }
}

#[tokio::test]
async fn checkpoint_from_another_pairing_starts_a_full_walk() {
    let (state, creds) = fixture();
    let mut previous = creds.clone(); previous.cloud_base_url = "https://previous.invalid".into();
    seed(&state, &previous, "99");
    let (client, paths, task) = server(vec![(200, page(vec![], "0", None))], |_| async {}).await;
    pull_changes(&state, &client, &creds, &PI_KEY).await.unwrap(); task.await.unwrap();
    assert!(!paths.lock().unwrap()[0].contains("since="));
    assert_eq!(checkpoint(&state).as_deref(), Some("0"));
}

#[tokio::test]
async fn concurrent_completed_walk_cannot_be_overwritten_by_old_response() {
    let (state, creds) = fixture(); seed(&state, &creds, "10");
    let item=charge(&state,1,"11");
    let changed = state.clone(); let current = creds.clone();
    let (client, _, task) = server(vec![(200, page(vec![item], "20", None))], move |_| {
        let changed = changed.clone(); let current = current.clone();
        async move {
            changed.store.set_charge_tags_from_sync(1,&["Newer completed value".into()]).unwrap();
            seed(&changed, &current, "30");
        }
    }).await;
    assert!(pull_changes(&state, &client, &creds, &PI_KEY).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("30"));
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Newer completed value"]);
}

#[tokio::test]
async fn repeated_reset_is_bounded_and_retains_previous_checkpoint() {
    let (state, creds) = fixture(); seed(&state, &creds, "10");
    let (client, paths, task) = server(vec![(409, json!({"error":"sync_reset_required"})),
        (409, json!({"error":"sync_reset_required"}))], |_| async {}).await;
    assert!(pull_changes(&state, &client, &creds, &PI_KEY).await.is_err()); task.await.unwrap();
    assert_eq!(checkpoint(&state).as_deref(), Some("10"));
    assert_eq!(paths.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn repeated_rate_limits_resume_saved_pages_instead_of_starving_later_changes() {
    let (state,creds)=fixture();
    let items:Vec<_>=(1..=3).map(|id|charge(&state,id,&id.to_string())).collect();
    for index in 0..3 {
        let next=if index<2 {Some(format!("page_{}",index+1))} else {None};
        let mut replies=vec![(200,page(vec![items[index].clone()],"3",next.as_deref()))];
        if next.is_some() {replies.push((429,json!({"error":"rate_limited"})));}
        let (client,paths,task)=server(replies,|_|async{}).await;
        let result=pull_changes(&state,&client,&creds,&PI_KEY).await;task.await.unwrap();
        assert_eq!(result.is_ok(),index==2);
        let requests=paths.lock().unwrap();
        if index==0 {assert!(!requests[0].contains("cursor="));}
        else {assert!(requests[0].contains(&format!("cursor=page_{index}")));}
        assert_eq!(state.store.get_charge_tags(index as i64+1).unwrap(),vec!["After"]);
        assert_eq!(checkpoint(&state),if index==2 {Some("3".into())} else {None});
    }
}

#[tokio::test]
async fn failed_cursor_persistence_replays_the_page_without_skipping_its_data() {
    let (state,creds)=fixture();let first=charge(&state,1,"1");let second=charge(&state,2,"2");
    state.store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_walk_cursor BEFORE INSERT ON meta WHEN NEW.key='cloud_mutable_revision_walk_v2' BEGIN SELECT RAISE(ABORT,'synthetic cursor save failure'); END;")).unwrap();
    let (client,_,task)=server(vec![(200,page(vec![first.clone()],"2",Some("second_page")))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(checkpoint(&state),None);
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,WALK_KEY)).unwrap().is_none());
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["After"]);
    state.store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_walk_cursor")).unwrap();
    let (client,paths,task)=server(vec![(200,page(vec![first],"2",Some("second_page"))),
        (200,page(vec![second],"2",None))],|_|async{}).await;
    pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(!paths.lock().unwrap()[0].contains("cursor="));assert_eq!(checkpoint(&state).as_deref(),Some("2"));
}

#[tokio::test]
async fn a_rescan_request_invalidates_a_saved_cursor_without_discarding_local_data() {
    let (state,creds)=fixture();seed(&state,&creds,"10");let item=charge(&state,1,"11");
    let (client,_,task)=server(vec![(200,page(vec![item.clone()],"20",Some("saved_page"))),
        (503,json!({"error":"unavailable"}))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    require_full_read(&state,&binding(&creds).unwrap()).unwrap();
    let (client,paths,task)=server(vec![(200,page(vec![item],"20",None))],|_|async{}).await;
    pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    let path=&paths.lock().unwrap()[0];assert!(!path.contains("since="));assert!(!path.contains("cursor="));
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["After"]);
    assert_eq!(checkpoint(&state).as_deref(),Some("20"));
}

#[tokio::test]
async fn malformed_saved_cursor_falls_back_to_the_last_complete_checkpoint() {
    let (state,creds)=fixture();seed(&state,&creds,"10");
    let saved=json!({"version":1,"binding":binding(&creds).unwrap(),
        "base":state.store.with_locked_conn(|conn|schema::meta_get(conn,CHECKPOINT_KEY)).unwrap(),
        "walk":{"since":"10","checkpoint":"20","cursor":"bad&since=999","last":[11,"charge",format!("{:064x}",1)]}}).to_string();
    state.store.with_locked_conn(|conn|schema::meta_set(conn,WALK_KEY,&saved)).unwrap();
    let (client,paths,task)=server(vec![(200,page(vec![],"20",None))],|_|async{}).await;
    pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(paths.lock().unwrap()[0],"/api/pi/sync/changes/v2?limit=200&since=10");
}

#[tokio::test]
async fn process_restart_resumes_the_saved_database_cursor() {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let suffix:String=nonce.iter().map(|byte|format!("{byte:02x}")).collect();
    let directory=std::env::temp_dir().join(format!("sentry-mutable-walk-{suffix}"));std::fs::create_dir(&directory).unwrap();
    let database=directory.join("synthetic.db");
    let (mut state,creds)=fixture();Arc::get_mut(&mut state).unwrap().store=Arc::new(crate::open_test_store(database.to_str().unwrap()).unwrap());
    let first=charge(&state,1,"1");let second=charge(&state,2,"2");
    let (client,_,task)=server(vec![(200,page(vec![first],"2",Some("after_restart"))),
        (503,json!({"error":"unavailable"}))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();drop(state);
    let (mut restarted,creds)=fixture();Arc::get_mut(&mut restarted).unwrap().store=Arc::new(crate::open_test_store(database.to_str().unwrap()).unwrap());
    let (client,paths,task)=server(vec![(200,page(vec![second],"2",None))],|_|async{}).await;
    pull_changes(&restarted,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(paths.lock().unwrap()[0].contains("cursor=after_restart"));
    assert_eq!(restarted.store.get_charge_tags(1).unwrap(),vec!["After"]);
    assert_eq!(restarted.store.get_charge_tags(2).unwrap(),vec!["After"]);
    assert_eq!(checkpoint(&restarted).as_deref(),Some("2"));
    drop(restarted);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn newer_in_progress_page_blocks_late_plaintext_even_before_checkpoint_completion() {
    let (state,creds)=fixture();seed(&state,&creds,"10");let item=charge(&state,1,"11");
    let changed=state.clone();let expected=binding(&creds).unwrap();
    let (client,_,task)=server(vec![(200,page(vec![item],"20",Some("older_cursor")))],move |_| {
        let changed=changed.clone();let expected=expected.clone();async move {
            changed.store.set_charge_tags_from_sync(1,&["Newer page value".into()]).unwrap();
            changed.store.with_locked_conn(|conn| {
                let resume=SavedWalk {version:1,binding:expected,base:schema::meta_get(conn,CHECKPOINT_KEY)?,
                    walk:Walk {since:Some("10".into()),checkpoint:Some("20".into()),cursor:Some("newer_cursor".into()),
                        last:Some((11,Kind::Charge,format!("{:064x}",1)))}};
                schema::meta_set(conn,WALK_KEY,&serde_json::to_string(&resume)?)
            }).unwrap();
        }
    }).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Newer page value"]);
    assert_eq!(checkpoint(&state).as_deref(),Some("10"));
    let saved=state.store.with_locked_conn(|conn|schema::meta_get(conn,WALK_KEY)).unwrap().unwrap();
    assert_eq!(serde_json::from_str::<SavedWalk>(&saved).unwrap().walk.cursor.as_deref(),Some("newer_cursor"));
}

#[tokio::test]
async fn old_revoke_and_rekey_cannot_delete_or_replace_a_new_pairing() {
    let (mut state,old)=fixture();
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let directory=std::env::temp_dir().join(format!("sentry-credential-race-{}",URL_SAFE_NO_PAD.encode(nonce)));
    std::fs::create_dir(&directory).unwrap();let path=directory.join("synthetic-credentials.json");
    Arc::get_mut(&mut state).unwrap().credentials_path=path.to_str().unwrap().into();
    let mut current=old.clone();current.pi_auth_token="synthetic-new-pairing".into();
    state.set_credentials(current.clone()).await.unwrap();let saved=std::fs::read(&path).unwrap();
    assert!(!state.handle_remote_revoke(&old).await);
    let mut rekeyed=old.clone();rekeyed.dek_rotation_generation=1;
    assert!(!state.replace_credentials_if_current(&old,rekeyed).await.unwrap());
    assert_eq!(state.creds.lock().await.as_ref(),Some(&current));assert_eq!(std::fs::read(&path).unwrap(),saved);
    assert!(state.handle_remote_revoke(&current).await);assert!(!path.exists());
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn credential_file_publication_waits_for_the_same_lock_as_in_memory_publication() {
    let (mut state,old)=fixture();
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let directory=std::env::temp_dir().join(format!("sentry-credential-publish-{}",URL_SAFE_NO_PAD.encode(nonce)));
    std::fs::create_dir(&directory).unwrap();let path=directory.join("synthetic-credentials.json");
    Arc::get_mut(&mut state).unwrap().credentials_path=path.to_str().unwrap().into();
    state.set_credentials(old.clone()).await.unwrap();let original=std::fs::read(&path).unwrap();
    let mut new=old.clone();new.pi_auth_token="synthetic-next-pairing".into();
    let guard=state.creds.lock().await;let mut publication=Box::pin(state.set_credentials(new.clone()));
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(publication.as_mut(),context).is_pending());
        std::task::Poll::Ready(())
    }).await;
    assert_eq!(std::fs::read(&path).unwrap(),original);
    drop(guard);publication.await.unwrap();
    assert_eq!(sentryusb_cloud_crypto::credentials::load(path.to_str().unwrap()).unwrap(),new);
    drop(state);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn null_charge_mutable_authenticates_and_backfills_unknown_key_before_clearing_fields() {
    for valid_key in [true, false] {
        let (state, creds) = fixture();
        let mut item = charge(&state, 1, "1");
        item["ciphertext"] = Value::Null;
        let id = item["id"].as_str().unwrap().to_string();
        let wrapped = item["wrappedKey"].as_str().unwrap().to_string();
        state.store.charge_upload_mark(1, &id, "", 10).unwrap();
        let (client, _, task) = server(vec![(200, page(vec![item], "1", None))], |_| async {}).await;
        let key = if valid_key { PI_KEY } else { [8; 32] };
        let result = pull_changes(&state, &client, &creds, &key).await;
        task.await.unwrap();
        assert_eq!(result.is_ok(), valid_key);
        let uploads = state.store.charge_uploads_map_since(0).unwrap();
        assert_eq!(uploads[&1].1, if valid_key { wrapped } else { String::new() });
        assert_eq!(uploads[&1].2, 10);
        assert_eq!(state.store.get_charge_tags(1).unwrap(), if valid_key { vec![] } else { vec!["Before"] });
        assert_eq!(checkpoint(&state).as_deref(),Some("1"));
        assert_eq!(retry_count(&state,&creds),if valid_key {0}else{1});
    }
}

fn retry_count(state:&CloudStateInner,creds:&CloudCredentialsV1)->i64 {
    state.store.with_locked_conn(|conn|crate::sync::incoming_retry::count(conn,&binding(creds).unwrap())).unwrap()
}

#[tokio::test]
async fn failed_record_does_not_block_other_records_and_retry_reads_latest_cloud_fields() {
    let (state,creds)=fixture();let mut bad=charge(&state,1,"1");let good=charge(&state,2,"2");
    bad["ciphertext"]=json!(crate::encrypt::seal_json_b64(&[11;32],&aad::charge_mutable("owner","pi",bad["id"].as_str().unwrap()),&json!({"tags":["Unreadable"]})).unwrap());
    let (client,_,task)=server(vec![(200,page(vec![bad.clone(),good],"2",None))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before"]);
    assert_eq!(state.store.get_charge_tags(2).unwrap(),vec!["After"]);
    assert_eq!(checkpoint(&state).as_deref(),Some("2"));assert_eq!(retry_count(&state,&creds),1);
    let entries=state.store.with_locked_conn(|conn|crate::sync::incoming_retry::entries(conn,&binding(&creds).unwrap())).unwrap();
    assert!(!entries[0].raw.contains(bad["ciphertext"].as_str().unwrap()),"retries must not retain stale encrypted payloads");
    bad["status"]=json!("ok");bad["recordVersion"]=json!("a".repeat(64));
    bad["ciphertext"]=json!(crate::encrypt::seal_json_b64(&CONTENT_KEY,&aad::charge_mutable("owner","pi",bad["id"].as_str().unwrap()),
        &json!({"tags":["Latest Cloud"],"costOverride":{"amount":7.0,"currency":"CAD"}})).unwrap());
    let (client,paths,task)=server(vec![(200,json!({"ok":true,"writeProtocol":3,"items":[bad]})),(200,page(vec![],"3",None))],|_|async{}).await;
    pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(paths.lock().unwrap().as_slice(),&["/api/pi/sync/state","/api/pi/sync/changes/v2?limit=200&since=2"]);
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Latest Cloud"]);
    assert_eq!(state.store.get_charge_cost(1).unwrap(),Some((7.0,"CAD".into())));
    assert_eq!(retry_count(&state,&creds),0);
}

#[tokio::test]
async fn failed_retry_persistence_cannot_advance_the_feed_cursor() {
    let (state,creds)=fixture();let mut bad=charge(&state,1,"1");let good=charge(&state,2,"2");
    bad["ciphertext"]=json!(crate::encrypt::seal_json_b64(&[11;32],&aad::charge_mutable("owner","pi",bad["id"].as_str().unwrap()),&json!({"tags":[]})).unwrap());
    state.store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_incoming BEFORE INSERT ON meta WHEN NEW.key LIKE 'cloud_incoming_retry_v2:%' BEGIN SELECT RAISE(ABORT,'synthetic retry persistence failure'); END;")).unwrap();
    let (client,_,task)=server(vec![(200,page(vec![bad,good],"2",None))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(checkpoint(&state),None);assert_eq!(retry_count(&state,&creds),0);
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before"]);
    assert_eq!(state.store.get_charge_tags(2).unwrap(),vec!["After"]);
}

#[tokio::test]
async fn incoming_retry_survives_database_reopen_after_the_feed_checkpoint_advances() {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let suffix:String=nonce.iter().map(|byte|format!("{byte:02x}")).collect();
    let directory=std::env::temp_dir().join(format!("sentry-incoming-retry-{suffix}"));std::fs::create_dir(&directory).unwrap();
    let path=directory.join("synthetic.db");
    let (mut state,creds)=fixture();Arc::get_mut(&mut state).unwrap().store=Arc::new(crate::open_test_store(path.to_str().unwrap()).unwrap());
    let mut item=charge(&state,1,"1");
    let (client,_,task)=server(vec![(200,page(vec![item.clone()],"1",None))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&[5;32]).await.is_err());task.await.unwrap();drop(state);
    let (mut reopened,creds)=fixture();Arc::get_mut(&mut reopened).unwrap().store=Arc::new(crate::open_test_store(path.to_str().unwrap()).unwrap());
    assert_eq!(retry_count(&reopened,&creds),1);assert_eq!(checkpoint(&reopened).as_deref(),Some("1"));
    item["status"]=json!("ok");item["recordVersion"]=json!("a".repeat(64));
    let (client,_,task)=server(vec![(200,json!({"ok":true,"writeProtocol":3,"items":[item]})),(200,page(vec![],"2",None))],|_|async{}).await;
    pull_changes(&reopened,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state_tags(&reopened),vec!["After"]);assert_eq!(retry_count(&reopened,&creds),0);
    drop(reopened);std::fs::remove_dir_all(directory).unwrap();
}
fn state_tags(state:&CloudStateInner)->Vec<String> {state.store.get_charge_tags(1).unwrap()}

#[tokio::test]
async fn one_unreadable_route_is_isolated_from_other_drives_and_charging() {
    let (state,creds)=fixture();let (_,mut bad)=crate::sync::route_pull::tests::route(&state,0,&[(1,20),(0,40)],json!(["Bad"]));
    let (_,good)=crate::sync::route_pull::tests::route(&state,10,&[(1,60)],json!(["Good"]));
    let charging=charge(&state,1,"3");
    let groups=state.store.with_route_summaries(sentryusb_drives::grouper::drive_key_clip_windows).unwrap();assert_eq!(groups.len(),2);
    for (drive,_) in &groups {state.store.set_drive_tags_from_sync(drive,&["Before".into()]).unwrap();}
    bad["ciphertext"]=json!(crate::encrypt::seal_json_b64(&[11;32],&aad::route_tags("owner","pi",bad["id"].as_str().unwrap()),&json!(["Bad"])).unwrap());
    let route_item=|row:&Value,revision:&str|json!({"kind":"route","id":row["id"],"revision":revision,"updatedAtMs":100,"ciphertext":row["ciphertext"],"wrappedKey":row["wrappedKey"]});
    let states=|rows:Vec<Value>|json!({"ok":true,"writeProtocol":3,"items":rows});
    let (client,paths,task)=server(vec![(200,page(vec![charging,route_item(&bad,"1"),route_item(&good,"2")],"3",None)),
        (200,states(vec![bad.clone(),good.clone()])),(200,states(vec![bad])),(200,states(vec![good]))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(paths.lock().unwrap().len(),4);
    let mut labels=groups.iter().map(|(drive,_)|state.store.get_drive_tags(drive).unwrap()).collect::<Vec<_>>();labels.sort();
    assert_eq!(labels,vec![vec!["Before"],vec!["Good"]]);assert_eq!(state_tags(&state),vec!["After"]);
    assert_eq!(retry_count(&state,&creds),1);assert_eq!(checkpoint(&state).as_deref(),Some("3"));
}

#[tokio::test]
async fn route_http_failure_saves_the_group_without_splitting_into_a_request_storm() {
    let (state,creds)=fixture();let (_,first)=crate::sync::route_pull::tests::route(&state,0,&[(1,20),(0,40)],json!(["First"]));
    let (_,second)=crate::sync::route_pull::tests::route(&state,10,&[(1,60)],json!(["Second"]));
    let item=|row:&Value|json!({"kind":"route","id":row["id"],"revision":"1","updatedAtMs":100,"ciphertext":row["ciphertext"],"wrappedKey":row["wrappedKey"]});
    let (client,paths,task)=server(vec![(200,page(vec![item(&first),item(&second)],"1",None)),(429,json!({"error":"rate_limited"}))],|_|async{}).await;
    assert!(pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(paths.lock().unwrap().len(),2);assert_eq!(retry_count(&state,&creds),2);
    assert_eq!(checkpoint(&state).as_deref(),Some("1"));
}

#[tokio::test]
async fn current_feed_can_finish_work_when_the_earlier_retry_read_was_unavailable() {
    let (state,creds)=fixture();let item=charge(&state,1,"1");
    let binding=binding(&creds).unwrap();
    state.store.with_locked_conn(|conn|crate::sync::incoming_retry::record(conn,&binding,
        &[crate::sync::incoming_retry::Target {kind:Kind::Charge,id:item["id"].as_str().unwrap().into()}],&[])).unwrap();
    let (client,_,task)=server(vec![(503,json!({"error":"unavailable"})),(200,page(vec![item],"1",None))],|_|async{}).await;
    pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(retry_count(&state,&creds),0);assert_eq!(state_tags(&state),vec!["After"]);
}

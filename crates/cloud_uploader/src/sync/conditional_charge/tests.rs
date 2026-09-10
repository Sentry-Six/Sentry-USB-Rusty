use super::*;
use crate::sync::revision::tests::{fixture,charge};
use tokio::{net::TcpListener,io::{AsyncReadExt,AsyncWriteExt}};
use std::time::Duration;
const PI_KEY: [u8;32]=[7;32];
const CONTENT_KEY: [u8;32]=[9;32];

fn setup() -> (Arc<CloudStateInner>,CloudCredentialsV1,Value) {
    let (state,creds)=fixture(); let mut remote=charge(&state,1,"1");
    remote["status"]=json!("ok"); remote["recordVersion"]=json!("a".repeat(64));
    state.store.set_charge_tags(1,&["Before".into(),"Work".into()]).unwrap();
    remote["ciphertext"]=json!(encrypt::seal_json_b64(&CONTENT_KEY,&aad::charge_mutable("owner","pi",remote["id"].as_str().unwrap()),
        &json!({"tags":["Before","Remote"],"costOverride":{"amount":8.0,"currency":"CAD","receipt":"keep"},"extra":true})).unwrap());
    (state,creds,remote)
}

// Only synthetic bodies are retained; the HTTP Authorization header is dropped.
pub(in crate::sync) async fn server<F>(count: usize, mut respond: F) -> (CloudClient,tokio::task::JoinHandle<()>)
where F: FnMut(&str,Value,usize)->(u16,Value)+Send+'static {
    let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
    let task=tokio::spawn(async move {
        for index in 0..count {
            let (mut socket,_)=tokio::time::timeout(Duration::from_secs(5),listener.accept()).await.unwrap().unwrap();
            let mut data=Vec::new();
            let end=loop {
                let mut buf=[0u8;4096];let n=socket.read(&mut buf).await.unwrap();assert!(n>0);data.extend_from_slice(&buf[..n]);
                if let Some(p)=data.windows(4).position(|w| w==b"\r\n\r\n") { break p+4 }
                assert!(data.len()<8192);
            };
            let header=std::str::from_utf8(&data[..end]).unwrap();
            let path=header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
            let len=header.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap();
            while data.len()<end+len {let mut buf=[0u8;4096];let n=socket.read(&mut buf).await.unwrap();assert!(n>0);data.extend_from_slice(&buf[..n]);}
            let (status,body)=respond(&path,serde_json::from_slice(&data[end..end+len]).unwrap(),index);
            let bytes=serde_json::to_vec(&body).unwrap();
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        }
    });
    (CloudClient::new(format!("http://{addr}")).with_bearer(&[3;32]),task)
}
fn response(remote: &Value)->Value { json!({"ok":true,"writeProtocol":3,"items":[remote]}) }
fn ack(remote: &Value,status: &str)->Value { json!({"ok":true,"writeProtocol":3,"results":[{"kind":"charge","id":remote["id"],"status":status}]}) }
fn open_proposal(item: &Value)->Value { encrypt::open_json_b64(&CONTENT_KEY,&aad::charge_mutable("owner","pi",item["id"].as_str().unwrap()),item["ciphertext"].as_str().unwrap()).unwrap() }

#[tokio::test]
async fn runtime_write_preserves_remote_cost_extensions_and_merges_newer_local_edit_on_ack() {
    let (state,creds,remote)=setup();let changed=state.clone();let expected=remote.clone();
    let (client,task)=server(2,move |path,body,index| {
        assert_eq!(body["piId"],"pi");assert_eq!(body["dekRotationGeneration"],0);
        if index==0 { assert_eq!(path,"/api/pi/sync/state");return (200,response(&remote)) }
        assert_eq!(path,"/api/pi/sync/mutables/v3");
        let item=&body["items"][0];assert_eq!(item["expectedCiphertext"],expected["ciphertext"]);
        assert_eq!(item["recordVersion"],expected["recordVersion"]);assert_eq!(item["wrappedKey"],expected["wrappedKey"]);
        assert_eq!(item["fieldMerge"],true);
        let merged=open_proposal(item);assert_eq!(merged["tags"],json!(["Before","Remote","Work"]));
        assert_eq!(merged["costOverride"],json!({"amount":8.0,"currency":"CAD","receipt":"keep"}));assert_eq!(merged["extra"],true);
        changed.store.set_charge_tags(1,&["Before".into(),"Later".into()]).unwrap();
        (200,ack(&remote,"applied"))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before","Later","Remote"]);
    assert_eq!(state.store.get_charge_cost(1).unwrap(),Some((8.0,"CAD".into())));
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    let pending=state.store.with_locked_conn(|c| sentryusb_drives::mutable_intent::read(c,"charge","1")).unwrap();
    assert_eq!(pending.edits.len(),1);
    assert!(state.store.with_locked_conn(|c|schema::meta_get(c,&receipt_key(&revision::binding(&creds).unwrap(),1))).unwrap().is_none());
}

#[tokio::test]
async fn unknown_write_result_recovers_exact_ciphertext_without_second_post() {
    let (state,creds,mut remote)=setup();
    let (client,task)=server(3,move |path,body,index| {
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");remote["ciphertext"]=body["items"][0]["ciphertext"].clone();
            return (503,json!({"error":"pi_mutable_save_unconfirmed"}));
        }
        assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before","Remote","Work"]);
}

#[tokio::test]
async fn unavailable_receipt_retry_retains_the_exact_original_operation() {
    let (state,creds,remote)=setup();let later=remote.clone();
    let (client,task)=server(3,move |path,_,index| {
        if index==1 {assert_eq!(path,"/api/pi/sync/mutables/v3");return (503,json!({"error":"unknown"}))}
        assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let receipt=receipt_key(&revision::binding(&creds).unwrap(),1);
    let raw=state.store.with_locked_conn(|c|schema::meta_get(c,&receipt)).unwrap().unwrap();
    let original: Pending=serde_json::from_str(&raw).unwrap();
    let (client,task)=server(2,move |path,body,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/state");return (200,response(&later))}
        assert_eq!(path,"/api/pi/sync/mutables/v3");
        assert_eq!(body["items"][0],serde_json::to_value(&original.proposal).unwrap());
        (503,json!({"error":"synthetic unavailable receipt"}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.with_locked_conn(|c|schema::meta_get(c,&receipt)).unwrap(),Some(raw));
    assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before","Work"]);
}

#[tokio::test]
async fn malformed_ack_is_read_back_before_any_local_confirmation() {
    let (state,creds,remote)=setup();
    let (client,task)=server(4,move |path,_,index| {
        if index==1 {return (200,json!({"ok":true,"writeProtocol":3,"results":[]}))}
        if index==3 {assert_eq!(path,"/api/pi/sync/mutables/v3");return (503,json!({"error":"synthetic unavailable receipt"}))}
        assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn definite_conflict_retains_intent_but_releases_attempt_for_fresh_merge() {
    let (state,creds,remote)=setup();
    let (client,task)=server(2,move |_,_,index| (200,if index==0 {response(&remote)} else {ack(&remote,"conflict")})).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let receipt=receipt_key(&revision::binding(&creds).unwrap(),1);
    assert!(state.store.with_locked_conn(|c|schema::meta_get(c,&receipt)).unwrap().is_none());
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn cost_conflict_and_unknown_legacy_edit_never_publish() {
    for legacy in [false,true] {
        let (state,creds,remote)=setup();
        if legacy {state.store.with_locked_conn(|c| c.execute("DELETE FROM mutable_intent_state",[])).unwrap();}
        else {state.store.set_charge_cost(1,Some((10.0,"CAD".into()))).unwrap();}
        let (client,task)=server(1,move |path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))}).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    }
}

#[tokio::test]
async fn missing_nullable_state_field_does_not_clear_or_publish() {
    let (state,creds,mut remote)=setup();remote.as_object_mut().unwrap().remove("ciphertext");
    let (client,task)=server(1,move |_,_,_| (200,response(&remote))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn replaced_pairing_cannot_acknowledge_a_write_from_previous_pairing() {
    let (state,creds,remote)=setup();let changed=state.clone();
    let (client,task)=server(2,move |_,_,index| {
        if index==0 {return (200,response(&remote))}
        changed.creds.try_lock().unwrap().as_mut().unwrap().pi_auth_token="replacement".into();
        (200,ack(&remote,"applied"))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn replaced_local_upload_does_not_adopt_remote_fields_or_clear_intent() {
    let (state,creds,remote)=setup();let changed=state.clone();
    let (client,task)=server(2,move |_,_,index| {
        if index==0 {return (200,response(&remote))}
        changed.store.charge_upload_mark(1,remote["id"].as_str().unwrap(),"replacement",2).unwrap();
        (200,ack(&remote,"applied"))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);assert_eq!(state.store.get_charge_cost(1).unwrap(),None);
}

#[tokio::test]
async fn charging_batches_cover_more_than_two_hundred_edits() {
    let (state,creds)=fixture();let mut remotes=std::collections::HashMap::new();
    for ts in 1..=201 {
        let mut remote=charge(&state,ts,"1");remote["status"]=json!("ok");remote["recordVersion"]=json!("a".repeat(64));
        state.store.set_charge_tags(ts,&["Work".into()]).unwrap();
        remotes.insert(remote["id"].as_str().unwrap().to_string(),remote);
    }
    let (client,task)=server(4,move |path,body,index| {
        let items=body["items"].as_array().unwrap();assert_eq!(items.len(),if index<2 {200} else {1});
        if index%2==0 {
            assert_eq!(path,"/api/pi/sync/state");
            (200,json!({"ok":true,"writeProtocol":3,"items":items.iter().map(|i|remotes[i["id"].as_str().unwrap()].clone()).collect::<Vec<_>>()}))
        } else {
            assert_eq!(path,"/api/pi/sync/mutables/v3");
            for item in items {assert_eq!(open_proposal(item)["tags"],json!(["After","Work"]));}
            (200,json!({"ok":true,"writeProtocol":3,"results":items.iter().map(|i|json!({"kind":"charge","id":i["id"],"status":"applied"})).collect::<Vec<_>>()}))
        }
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert_eq!(state.store.get_charge_tags(201).unwrap(),vec!["After","Work"]);
}

#[tokio::test]
async fn proposal_storage_failure_prevents_the_network_write() {
    let (state,creds,remote)=setup();
    state.store.with_locked_conn(|c|c.execute_batch("CREATE TRIGGER fail_publication BEFORE INSERT ON meta WHEN NEW.key LIKE 'cloud_charge_publication_v2:%' BEGIN SELECT RAISE(ABORT, 'synthetic persistence failure'); END;")).unwrap();
    let (client,task)=server(1,move |path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn matching_legacy_fields_reconcile_without_writing_a_replacement_envelope() {
    let (state,creds,mut remote)=setup();
    state.store.with_locked_conn(|c|c.execute("DELETE FROM mutable_intent_state",[])).unwrap();
    remote["ciphertext"]=json!(encrypt::seal_json_b64(&CONTENT_KEY,&aad::charge_mutable("owner","pi",remote["id"].as_str().unwrap()),
        &json!({"tags":["Before","Work"],"future":"preserved on Cloud"})).unwrap());
    let (client,task)=server(1,move |path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))}).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[tokio::test]
async fn reopening_the_database_recovers_the_original_encrypted_attempt() {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let name:String=nonce.iter().map(|b|format!("{b:02x}")).collect();
    let directory=std::env::temp_dir().join(format!("sentry-cloud-charge-restart-{name}"));
    std::fs::create_dir(&directory).unwrap();let db=directory.join("synthetic.db");
    let (mut state,creds)=fixture();
    Arc::get_mut(&mut state).unwrap().store=Arc::new(crate::open_test_store(db.to_str().unwrap()).unwrap());
    let mut remote=charge(&state,1,"1");remote["status"]=json!("ok");remote["recordVersion"]=json!("a".repeat(64));
    state.store.set_charge_tags(1,&["Work".into()]).unwrap();
    let saved=Arc::new(std::sync::Mutex::new(remote));let server_saved=saved.clone();
    let (client,task)=server(3,move |path,body,index| {
        if index==0 {return (200,response(&server_saved.lock().unwrap()))}
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");
            server_saved.lock().unwrap()["ciphertext"]=body["items"][0]["ciphertext"].clone();
        } else {assert_eq!(path,"/api/pi/sync/state");}
        (503,json!({"error":"synthetic lost reply"}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();drop(state);
    let (mut restarted,creds)=fixture();
    Arc::get_mut(&mut restarted).unwrap().store=Arc::new(crate::open_test_store(db.to_str().unwrap()).unwrap());
    let (client,task)=server(1,move |path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,response(&saved.lock().unwrap()))}).await;
    push(&restarted,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(restarted.store.dirty_mutables().unwrap().is_empty());
    assert_eq!(restarted.store.get_charge_tags(1).unwrap(),vec!["After","Work"]);
    drop(restarted);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn receipt_recovery_handles_an_unreceived_request_and_preserves_a_later_cloud_edit() {
    for already_committed in [false,true] {
        let (state,creds,remote)=setup();let original_remote=remote.clone();
        let (client,task)=server(3,move |path,_,index| {
            if index==0 {return (200,response(&remote))}
            if index==1 {assert_eq!(path,"/api/pi/sync/mutables/v3");}
            else {assert_eq!(path,"/api/pi/sync/state");}
            (503,json!({"error":"synthetic unavailable outcome"}))
        }).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        let receipt=receipt_key(&revision::binding(&creds).unwrap(),1);
        let raw=state.store.with_locked_conn(|c|schema::meta_get(c,&receipt)).unwrap().unwrap();
        let pending:Pending=serde_json::from_str(&raw).unwrap();
        assert_eq!(pending.version,3);assert!(pending.proposal.operation_id.as_deref().is_some_and(hex_id));
        let mut remote=original_remote;
        if already_committed {
            remote["ciphertext"]=json!(encrypt::seal_json_b64(&CONTENT_KEY,&aad::charge_mutable("owner","pi",remote["id"].as_str().unwrap()),
                &json!({"tags":["CloudLater"],"costOverride":{"amount":12.0,"currency":"CAD"}})).unwrap());
            state.store.set_charge_tags(1,&["Before".into(),"Work".into(),"LocalLater".into()]).unwrap();
        }
        let (client,task)=server(3,move |path,body,index| {
            if index==1 {
                assert_eq!(path,"/api/pi/sync/mutables/v3");
                assert_eq!(body["items"][0],serde_json::to_value(&pending.proposal).unwrap());
                if !already_committed {remote["ciphertext"]=body["items"][0]["ciphertext"].clone();}
                return (200,ack(&remote,"applied"));
            }
            assert_eq!(path,"/api/pi/sync/state");(200,response(&remote))
        }).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
        assert!(state.store.with_locked_conn(|c|schema::meta_get(c,&receipt)).unwrap().is_none());
        if already_committed {
            assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["CloudLater","LocalLater"]);
            assert_eq!(state.store.get_charge_cost(1).unwrap(),Some((12.0,"CAD".into())));
            assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
        } else {
            assert_eq!(state.store.get_charge_tags(1).unwrap(),vec!["Before","Remote","Work"]);
            assert_eq!(state.store.get_charge_cost(1).unwrap(),Some((8.0,"CAD".into())));
            assert!(state.store.dirty_mutables().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn state_without_receipt_protocol_never_reaches_the_writer() {
    let (state,creds,remote)=setup();let mut old_response=response(&remote);
    old_response.as_object_mut().unwrap().remove("writeProtocol");
    let (client,task)=server(1,move |path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,old_response.clone())}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn duplicate_unknown_key_is_backfilled_only_after_authenticated_cloud_state() {
    let (state,creds,remote)=setup();
    state.store.with_locked_conn(|conn|conn.execute("UPDATE charge_uploads SET wrapped_charge_key='' WHERE session_ts=1",[])).unwrap();
    let expected=remote["wrappedKey"].as_str().unwrap().to_string();
    let (client,task)=server(2,move |_,_,index|(200,if index==0 {response(&remote)} else {ack(&remote,"applied")})).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    let key:String=state.store.with_locked_conn(|conn|conn.query_row("SELECT wrapped_charge_key FROM charge_uploads WHERE session_ts=1",[],|row|row.get(0))).unwrap();
    assert_eq!(key,expected);
}

#[tokio::test]
async fn failed_key_authentication_does_not_replace_an_unknown_duplicate_key() {
    let (state,creds,remote)=setup();
    state.store.with_locked_conn(|conn|conn.execute("UPDATE charge_uploads SET wrapped_charge_key='' WHERE session_ts=1",[])).unwrap();
    let (client,task)=server(1,move |_,_,_|(200,response(&remote))).await;
    assert!(push(&state,&client,&creds,&[4;32]).await.is_err());task.await.unwrap();
    let key:String=state.store.with_locked_conn(|conn|conn.query_row("SELECT wrapped_charge_key FROM charge_uploads WHERE session_ts=1",[],|row|row.get(0))).unwrap();
    assert_eq!(key,"");assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

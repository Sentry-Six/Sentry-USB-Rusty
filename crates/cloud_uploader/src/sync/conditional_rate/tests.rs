use super::*;
use std::sync::Mutex;
use sentryusb_drives::{DriveStore, mutable_intent::RATE_PREFERENCES_JOURNAL, rate_confirmation::Confirmation};
use crate::state::RateConfigAccess;
use crate::sync::{revision::tests::fixture, conditional_charge::tests::server};
const PI_KEY:[u8;32]=[7;32];
struct Rates(Mutex<Value>);
impl RateConfigAccess for Rates {
    fn load_doc(&self,_:&DriveStore)->Result<Value> {Ok(self.0.lock().unwrap().clone())}
    fn queue_initial_doc(&self,store:&DriveStore)->Result<()> {
        store.queue_initial_rate_config(&self.0.lock().unwrap())?;Ok(())
    }
    fn store_doc(&self,_:&Value,_:&DriveStore,_:i64)->Result<()> {anyhow::bail!("unexpected incoming call")}
    fn confirm_doc(&self,doc:&Value,store:&DriveStore,through:i64,receipt:Option<(&str,&str)>)->Result<()> {
        let mut current=self.0.lock().unwrap();let snapshot=store.rate_sync_snapshot()?;
        let merged=rate_intent::replay_after(&snapshot.intent,through,doc)?;
        let confirmation=Confirmation {through,snapshot,receipt:receipt.map(|(key,raw)|(key.into(),raw.into()))};
        store.stage_rate_confirmation(&confirmation,"synthetic file confirmation")?;
        *current=merged;
        store.finish_rate_confirmation(&confirmation,"synthetic file confirmation")
    }
}
fn edit(state:&CloudStateInner,rates:&Rates,next:Value) {
    let mut current=rates.0.lock().unwrap();
    state.store.stage_rate_preferences(&current,&next,false,"synthetic local file publication").unwrap();
    *current=next;
    state.store.with_locked_conn(|conn|schema::meta_del(conn,RATE_PREFERENCES_JOURNAL)).unwrap();
}
fn remote(doc:Value)->Value {
    json!({"status":"ok","kind":"rateConfig","id":"pi","recordVersion":null,"wrappedKey":null,"updatedAtMs":1,
        "ciphertext":encrypt::seal_json_b64(&PI_KEY,&aad::rate_config("owner","pi"),&doc).unwrap()})
}
fn response(remote:&Value)->Value {json!({"ok":true,"writeProtocol":3,"items":[remote]})}
fn ack(status:&str)->Value {json!({"ok":true,"writeProtocol":3,"results":[{"kind":"rateConfig","id":"pi","status":status}]})}
fn setup()->(Arc<CloudStateInner>,CloudCredentialsV1,Arc<Rates>,Value) {
    let (mut state,creds)=fixture();let rates=Arc::new(Rates(Mutex::new(json!({"charging_default_rate":0.1}))));
    Arc::get_mut(&mut state).unwrap().rate_config=Some(rates.clone());
    edit(&state,&rates,json!({"charging_default_rate":0.2}));
    let current=remote(json!({"charging_default_rate":0.1,"charging_currency":"CAD","extension":true}));
    (state,creds,rates,current)
}
#[tokio::test]
async fn field_merge_preserves_cloud_values_and_later_local_edits() {
    let (state,creds,rates,mut current)=setup();let changed=state.clone();let local=rates.clone();
    let (client,task)=server(3,move |path,body,index| {
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");let item=&body["items"][0];
            assert_eq!(item["fieldMerge"],true);assert_eq!(item["recordVersion"],Value::Null);assert_eq!(item["wrappedKey"],Value::Null);
            assert!(hex(item["operationId"].as_str().unwrap()));assert_eq!(item["expectedCiphertext"],current["ciphertext"]);
            let merged=decrypt(&creds,&PI_KEY,item["ciphertext"].as_str()).unwrap();
            assert_eq!(merged,json!({"charging_default_rate":0.2,"charging_currency":"CAD","extension":true}));
            edit(&changed,&local,json!({"charging_default_rate":0.3}));
            current["ciphertext"]=item["ciphertext"].clone();return (200,ack("applied"));
        }
        assert_eq!(path,"/api/pi/sync/state");(200,response(&current))
    }).await;
    let creds=state.creds.lock().await.clone().unwrap();
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.3,"charging_currency":"CAD","extension":true}));
    assert_eq!(state.store.rate_sync_snapshot().unwrap().intent.edits.len(),1);
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(&revision::binding(&creds).unwrap()))).unwrap().is_none());
}
#[tokio::test]
async fn unknown_result_uses_exact_readback_without_publishing_again() {
    let (state,creds,_,mut current)=setup();
    let (client,task)=server(3,move |path,body,index| {
        if index==1 {assert_eq!(path,"/api/pi/sync/mutables/v3");current["ciphertext"]=body["items"][0]["ciphertext"].clone();return (503,json!({"error":"unconfirmed"}))}
        (200,response(&current))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();assert!(state.store.dirty_mutables().unwrap().is_empty());
}
#[tokio::test]
async fn receipt_retry_reuses_original_proposal_and_adopts_newer_cloud_fields() {
    let (state,creds,rates,current)=setup();
    let (client,task)=server(3,move |_,_,index| match index {0=>(200,response(&current)),1=>(200,ack("applied")),_=>(503,json!({"error":"offline"}))}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let key=receipt_key(&revision::binding(&creds).unwrap());let raw=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().unwrap();
    let pending:Pending=serde_json::from_str(&raw).unwrap();
    let later=remote(json!({"charging_default_rate":0.25,"charging_currency":"CAD","extension":"newer"}));
    let (client,task)=server(3,move |path,body,index| {
        if index==1 {assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(body["items"][0],serde_json::to_value(&pending.proposal).unwrap());return (200,ack("applied"))}
        (200,response(&later))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.25,"charging_currency":"CAD","extension":"newer"}));
    assert!(state.store.dirty_mutables().unwrap().is_empty());assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().is_none());
}
#[tokio::test]
async fn rejected_write_preserves_intent_for_fresh_merge() {
    let (state,creds,_,current)=setup();
    let (client,task)=server(2,move |_,_,index|(200,if index==0 {response(&current)}else{ack("conflict")})).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(&revision::binding(&creds).unwrap()))).unwrap().is_none());
}
#[tokio::test]
async fn unreadable_state_or_competing_price_cannot_publish() {
    for invalid in ["missing","foreign","price","ciphertext"] {
        let (state,creds,_,mut current)=setup();
        match invalid {
            "missing"=>{current.as_object_mut().unwrap().remove("wrappedKey");},
            "foreign"=>current["id"]=json!("other-pi"),
            "price"=>current=remote(json!({"charging_default_rate":0.3})),
            _=>current["ciphertext"]=json!("broken"),
        }
        let (client,task)=server(1,move |_,_,_|(200,response(&current))).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    }
}
#[tokio::test]
async fn replaced_pairing_after_post_cannot_confirm_or_retire_the_old_edit() {
    let (state,creds,_,current)=setup();let changed=state.clone();
    let (client,task)=server(2,move |_,_,index| {
        if index==0 {return (200,response(&current))}
        changed.creds.try_lock().unwrap().as_mut().unwrap().pi_auth_token="replacement".into();(200,ack("applied"))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

fn empty_target()->Value {
    json!({"status":"ok","kind":"rateConfig","id":"pi","recordVersion":null,
        "wrappedKey":null,"updatedAtMs":0,"ciphertext":null})
}

fn unqueued(doc:Value)->(Arc<CloudStateInner>,CloudCredentialsV1,Arc<Rates>) {
    let (mut state,creds)=fixture();let rates=Arc::new(Rates(Mutex::new(doc)));
    Arc::get_mut(&mut state).unwrap().rate_config=Some(rates.clone());
    (state,creds,rates)
}

#[tokio::test]
async fn pairing_publishes_existing_configuration_without_a_new_local_edit() {
    let desired=json!({"charging_currency":"CAD","charging_tag_rates":{"Home":0.12}});
    let (state,creds,rates)=unqueued(desired.clone());let expected=desired.clone();
    let mut current=empty_target();
    let (client,task)=server(4,move |path,body,index| {
        if index==2 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");
            let item=&body["items"][0];assert_eq!(item["expectedCiphertext"],Value::Null);
            assert_eq!(decrypt(&creds,&PI_KEY,item["ciphertext"].as_str()).unwrap(),expected);
            current["ciphertext"]=item["ciphertext"].clone();return (200,ack("applied"));
        }
        (200,response(&current))
    }).await;
    let creds=state.creds.lock().await.clone().unwrap();
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),desired);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    // The server is stopped: a repeated sweep must not issue another request.
    push(&state,&client,&creds,&PI_KEY).await.unwrap();
}

#[tokio::test]
async fn existing_cloud_configuration_is_not_initialized_from_local_preferences() {
    let (state,creds,rates)=unqueued(json!({"charging_default_rate":0.1}));
    let existing=remote(json!({"charging_default_rate":0.9}));
    let (client,task)=server(1,move |_,_,_|(200,response(&existing))).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.1}));
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    push(&state,&client,&creds,&PI_KEY).await.unwrap();
}

#[tokio::test]
async fn empty_local_configuration_needs_no_upload_or_repeated_probe() {
    let (state,creds,_)=unqueued(json!({}));
    let (client,task)=server(1,move |_,_,_|(200,response(&empty_target()))).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    push(&state,&client,&creds,&PI_KEY).await.unwrap();
}

#[tokio::test]
async fn completed_initial_check_does_not_suppress_a_later_local_edit() {
    let (state,creds,rates)=unqueued(json!({}));let mut current=empty_target();
    let (client,task)=server(4,move |_,body,index| {
        if index==2 {
            current["ciphertext"]=body["items"][0]["ciphertext"].clone();
            return (200,ack("applied"));
        }
        (200,response(&current))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();
    edit(&state,&rates,json!({"charging_default_rate":0.2}));
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.2}));
}

#[tokio::test]
async fn cloud_edit_between_initial_probe_and_publication_is_not_overwritten() {
    let (state,creds,rates)=unqueued(json!({"charging_default_rate":0.1}));
    let other=remote(json!({"charging_default_rate":0.9}));
    let (client,task)=server(2,move |_,_,index| {
        (200,response(&if index==0 {empty_target()} else {other.clone()}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.1}));
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn local_edit_during_initial_probe_is_preserved_instead_of_requeued() {
    let (state,creds,rates)=unqueued(json!({"charging_default_rate":0.1}));
    let changed=state.clone();let local=rates.clone();
    let (client,task)=server(1,move |_,_,_| {
        edit(&changed,&local,json!({"charging_default_rate":0.2}));
        (200,response(&empty_target()))
    }).await;
    prepare_initial(&state,&client,&creds,&PI_KEY,&revision::binding(&creds).unwrap()).await.unwrap();
    task.await.unwrap();
    let snapshot=state.store.rate_sync_snapshot().unwrap();
    assert_eq!(snapshot.intent.edits.len(),1);
    let sentryusb_drives::mutable_intent::Edit::RateConfig {before,after,..}=&snapshot.intent.edits[0].1 else {panic!("wrong intent")};
    assert_eq!(*before,json!({"charging_default_rate":0.1}));
    assert_eq!(*after,json!({"charging_default_rate":0.2}));
}

#[tokio::test]
async fn replacement_pairing_during_initial_probe_cannot_queue_or_checkpoint() {
    let (state,creds,_)=unqueued(json!({"charging_default_rate":0.1}));let changed=state.clone();
    let (client,task)=server(1,move |_,_,_| {
        changed.creds.try_lock().unwrap().as_mut().unwrap().pi_auth_token="replacement".into();
        (200,response(&empty_target()))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&initial_key(&revision::binding(&creds).unwrap()))).unwrap().is_none());
}

#[tokio::test]
async fn new_credentials_have_their_own_initial_checkpoint() {
    let (state,creds,_)=unqueued(json!({}));let old_binding=revision::binding(&creds).unwrap();
    state.store.with_durable_conn(|conn|schema::meta_set(conn,&initial_key(&old_binding),"1")).unwrap();
    let mut replacement=creds;replacement.pi_auth_token="replacement".into();
    *state.creds.lock().await=Some(replacement.clone());
    let (client,task)=server(1,move |_,_,_|(200,response(&empty_target()))).await;
    push(&state,&client,&replacement,&PI_KEY).await.unwrap();task.await.unwrap();
    let key=initial_key(&revision::binding(&replacement).unwrap());
    assert_eq!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().as_deref(),Some("1"));
}

#[tokio::test]
async fn failed_initial_probe_does_not_checkpoint_or_create_an_edit() {
    let (state,creds,_)=unqueued(json!({"charging_default_rate":0.1}));
    let (client,task)=server(1,move |_,_,_|(503,json!({"error":"offline"}))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    let key=initial_key(&revision::binding(&creds).unwrap());
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().is_none());
}

#[tokio::test]
async fn checkpoint_failure_leaves_the_durable_initial_edit_available_for_retry() {
    let (state,creds,rates)=unqueued(json!({"charging_default_rate":0.12}));
    state.store.with_locked_conn(|conn|conn.execute_batch(
        "CREATE TRIGGER fail_initial_checkpoint BEFORE INSERT ON meta WHEN NEW.key LIKE 'cloud_rate_initial_checked_v1:%' BEGIN SELECT RAISE(ABORT,'synthetic checkpoint failure'); END;"
    )).unwrap();
    let (client,task)=server(1,move |_,_,_|(200,response(&empty_target()))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.rate_sync_snapshot().unwrap().intent.edits.len(),1);
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.12}));
    state.store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_initial_checkpoint")).unwrap();
    let mut current=empty_target();
    let (client,task)=server(3,move |_,body,index| {
        if index==1 {
            current["ciphertext"]=body["items"][0]["ciphertext"].clone();
            return (200,ack("applied"));
        }
        (200,response(&current))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[tokio::test]
async fn orphaned_publication_cannot_be_overwritten_by_initialization() {
    let (state,creds,_)=unqueued(json!({"charging_default_rate":0.12}));
    let key=receipt_key(&revision::binding(&creds).unwrap());
    state.store.with_durable_conn(|conn|schema::meta_set(conn,&key,"synthetic retained operation")).unwrap();
    let client=CloudClient::new("http://127.0.0.1:1").with_bearer(&[3;32]);
    let error=push(&state,&client,&creds,&PI_KEY).await.unwrap_err();
    assert!(error.to_string().contains("no local queue"));
    assert_eq!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key)).unwrap().as_deref(),Some("synthetic retained operation"));
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[tokio::test]
async fn first_publication_includes_rates_saved_before_the_queued_edit() {
    for legacy in [false,true] {
        let (mut state,creds)=fixture();
        let initial=json!({"charging_currency":"CAD","charging_default_rate":0.1,
            "charging_tag_rates":{"Home":{"flat":0.12,"extension":true}}});
        let rates=Arc::new(Rates(Mutex::new(initial.clone())));
        Arc::get_mut(&mut state).unwrap().rate_config=Some(rates.clone());
        let mut desired=initial;desired["charging_default_rate"]=json!(0.2);
        if legacy {*rates.0.lock().unwrap()=desired.clone();state.store.mark_rate_config_dirty().unwrap();}
        else {edit(&state,&rates,desired.clone());}
        let expected=desired.clone();let mut current=empty_target();
        let (client,task)=server(3,move |path,body,index| {
            if index==1 {
                assert_eq!(path,"/api/pi/sync/mutables/v3");
                let item=&body["items"][0];assert_eq!(item["expectedCiphertext"],Value::Null);
                assert_eq!(decrypt(&creds,&PI_KEY,item["ciphertext"].as_str()).unwrap(),expected);
                current["ciphertext"]=item["ciphertext"].clone();return (200,ack("applied"));
            }
            (200,response(&current))
        }).await;
        let creds=state.creds.lock().await.clone().unwrap();
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
        assert_eq!(*rates.0.lock().unwrap(),desired);
        assert!(state.store.dirty_mutables().unwrap().is_empty());
    }
}

#[tokio::test]
async fn competing_first_publication_does_not_turn_into_an_unconditional_retry() {
    let (state,creds,_,_)=setup();
    let (client,task)=server(2,move |_,body,index| {
        if index==0 {return (200,response(&empty_target()))}
        assert_eq!(body["items"][0]["expectedCiphertext"],Value::Null);
        (200,ack("conflict"))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let other=remote(json!({"charging_default_rate":0.9,"charging_currency":"USD"}));
    let (client,task)=server(1,move |_,_,_|(200,response(&other))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn encrypted_empty_rates_are_not_a_missing_initial_document() {
    let (state,creds,_,_)=setup();let existing=remote(json!({}));
    let (client,task)=server(1,move |_,_,_|(200,response(&existing))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn first_publication_refuses_unrelated_preferences() {
    let (state,creds,rates,_)=setup();
    rates.0.lock().unwrap()["unrelated_private_preference"]=json!("synthetic");
    let (client,task)=server(1,move |_,_,_|(200,response(&empty_target()))).await;
    let error=push(&state,&client,&creds,&PI_KEY).await.unwrap_err();task.await.unwrap();
    assert!(error.to_string().contains("unrelated preferences"));
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn unreadable_initial_plans_remain_local_and_queued() {
    for plans in [json!("broken JSON"),json!([]),json!({"Home":true})] {
        let (state,creds,rates,_)=setup();
        rates.0.lock().unwrap()["charging_tag_rates"]=plans;
        let (client,task)=server(1,move |_,_,_|(200,response(&empty_target()))).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    }
}

#[tokio::test]
async fn uncertain_first_publication_keeps_newer_local_rates() {
    let (state,creds,rates,_)=setup();let changed=state.clone();let local=rates.clone();
    let mut current=empty_target();
    let (client,task)=server(3,move |_,body,index| {
        if index==1 {
            current["ciphertext"]=body["items"][0]["ciphertext"].clone();
            edit(&changed,&local,json!({"charging_default_rate":0.3}));
            return (503,json!({"error":"unconfirmed"}));
        }
        (200,response(&current))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(*rates.0.lock().unwrap(),json!({"charging_default_rate":0.3}));
    assert_eq!(state.store.rate_sync_snapshot().unwrap().intent.edits.len(),1);
}

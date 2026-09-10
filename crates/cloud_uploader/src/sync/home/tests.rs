use super::*;
use base64::{Engine as _,engine::general_purpose::{STANDARD,URL_SAFE_NO_PAD}};
use std::sync::Mutex;
use crate::{state::RateConfigAccess,sync::{revision::tests::fixture,conditional_charge::tests::server}};
use sentryusb_cloud_crypto::aead;
const PI_KEY:[u8;32]=[7;32];const KEY:[u8;32]=[9;32];
struct Config(Mutex<Option<HomeGeofence>>);
impl RateConfigAccess for Config {
    fn home_geofence(&self)->Result<Option<HomeGeofence>> {Ok(*self.0.lock().unwrap())}
    fn load_doc(&self,_:&sentryusb_drives::DriveStore)->Result<Value> {Ok(json!({}))}
    fn store_doc(&self,_:&Value,_:&sentryusb_drives::DriveStore,_:i64)->Result<()> {Ok(())}
}
fn setup()->(Arc<CloudStateInner>,CloudCredentialsV1,Arc<Config>) {
    let (mut state,creds)=fixture();let config=Arc::new(Config(Mutex::new(Some(HomeGeofence::new(0.0,0.0,120.0).unwrap()))));
    Arc::get_mut(&mut state).unwrap().rate_config=Some(config.clone());(state,creds,config)
}
fn source(ts:i64,at:i64,home:Option<bool>)->Value {
    let id=ids::charge_id_from_start_ts(ts);
    let wrapped=STANDARD.encode(aead::seal(&aead::Key::from_bytes(&PI_KEY).unwrap(),&aad::charge_key("owner","pi",&id),&KEY).unwrap());
    let summary=encrypt::seal_json_b64(&KEY,&aad::charge_summary("owner","pi",&id),&json!({"id":ts,"startMs":ts*1000,"locationLat":0.0,"locationLon":0.0})).unwrap();
    let mut document=json!({"tags":["Work"],"costOverride":{"amount":4.2,"currency":"CAD"},"extension":true});
    if let Some(home)=home {document["atHome"]=json!(home)}
    json!({"kind":"charge","id":id,"status":"ok","recordVersion":"a".repeat(64),"wrappedKey":wrapped,"summaryCiphertext":summary,
        "ciphertext":encrypt::seal_json_b64(&KEY,&aad::charge_mutable("owner","pi",&id),&document).unwrap(),"revision":at.to_string(),"updatedAtMs":1})
}
fn page(items:Vec<Value>,upper:i64,next:Option<String>)->Value {json!({"ok":true,"writeProtocol":3,"items":items,"sync":{"version":1,"revision":upper.to_string()},"nextCursor":next})}
fn states(items:Vec<Value>)->Value {json!({"ok":true,"writeProtocol":3,"items":items})}
fn ack(body:&Value,status:&str)->Value {json!({"ok":true,"writeProtocol":3,"results":body["items"].as_array().unwrap().iter().map(|item|json!({"kind":"charge","id":item["id"],"status":status})).collect::<Vec<_>>()})}
fn open(item:&Value)->Value {encrypt::open_json_b64(&KEY,&aad::charge_mutable("owner","pi",item["id"].as_str().unwrap()),item["ciphertext"].as_str().unwrap()).unwrap()}
fn cursor(last:&Value,upper:i64,since:Option<&str>)->String {URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"v":1,"userId":"owner","piId":"pi","generation":0,"since":since,
    "revision":upper.to_string(),"at":last["revision"],"id":last["id"]})).unwrap())}
#[tokio::test]
async fn sources_absent_from_the_local_database_are_classified_and_user_fields_survive() {
    let (state,creds,_)=setup();let remote=source(1,1,None);let original=remote.clone();
    let (client,task)=server(2,move|path,body,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/charge-states");return(200,page(vec![remote.clone()],1,None))}
        assert_eq!(path,"/api/pi/sync/mutables/v3");let item=&body["items"][0];assert_eq!(item["expectedSummaryCiphertext"],original["summaryCiphertext"]);
        assert_eq!(item["recordVersion"],original["recordVersion"]);assert_eq!(item["wrappedKey"],original["wrappedKey"]);
        assert_eq!(open(item),json!({"tags":["Work"],"costOverride":{"amount":4.2,"currency":"CAD"},"extension":true,"atHome":true}));
        (200,ack(&body,"applied"))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.charge_uploads_map().unwrap().is_empty());assert!(!store::remaining(&state.store,&revision::binding(&creds).unwrap()).unwrap());
}
#[tokio::test]
async fn an_unreadable_record_does_not_block_later_pages_or_good_records() {
    let (state,creds,_)=setup();let mut bad=source(1,1,None);bad["summaryCiphertext"]=json!("broken");let good=source(2,2,None);
    let next=cursor(&bad,2,None);
    let (client,task)=server(3,move|path,body,index|match index {
        0=>(200,page(vec![bad.clone()],2,Some(next.clone()))),
        1=>{assert_eq!(body["cursor"],next);(200,page(vec![good.clone()],2,None))},
        _=>{assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(body["items"].as_array().unwrap().len(),1);assert_eq!(body["items"][0]["id"],good["id"]);(200,ack(&body,"applied"))},
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let binding=revision::binding(&creds).unwrap();assert_eq!(store::retries(&state.store,&binding).unwrap(),vec![ids::charge_id_from_start_ts(1)]);
    let walk:Walk=serde_json::from_str(&store::load(&state.store,&binding).unwrap().unwrap()).unwrap();assert_eq!(walk.since.as_deref(),Some("2"));assert!(walk.cursor.is_none());
}
#[tokio::test]
async fn unknown_success_is_read_back_without_a_second_write_after_restart_of_the_pass() {
    let (state,creds,_)=setup();let remote=source(1,1,None);let shared=Arc::new(Mutex::new(remote));let published=shared.clone();
    let (client,task)=server(2,move|_,body,index| {
        if index==0 {return(200,page(vec![published.lock().unwrap().clone()],1,None))}
        published.lock().unwrap()["ciphertext"]=body["items"][0]["ciphertext"].clone();(503,json!({"error":"unknown"}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    let (client,task)=server(2,move|path,body,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/state");return(200,states(vec![shared.lock().unwrap().clone()]))}
        assert_eq!(path,"/api/pi/sync/charge-states");assert_eq!(body["since"],"1");(200,page(vec![],2,None))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();assert!(!store::remaining(&state.store,&revision::binding(&creds).unwrap()).unwrap());
}
#[tokio::test]
async fn changed_home_policy_uses_a_fresh_barrier_instead_of_replaying_an_old_unknown_write() {
    let (state,creds,config)=setup();let remote=source(1,1,Some(false));let retained=remote.clone();
    let (client,task)=server(2,move|_,_,index|if index==0{(200,page(vec![remote.clone()],1,None))}else{(503,json!({"error":"unknown"}))}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();let binding=revision::binding(&creds).unwrap();let id=retained["id"].as_str().unwrap();
    let original:Pending=serde_json::from_str(&store::attempt(&state.store,&binding,id).unwrap().unwrap()).unwrap();
    *config.0.lock().unwrap()=Some(HomeGeofence::new(1.0,1.0,120.0).unwrap());
    let original_id=original.proposal["operationId"].clone();let mut current=retained;
    let (client,task)=server(3,move|path,body,index|match index {
        0=>(200,states(vec![current.clone()])),
        1=>{assert_eq!(path,"/api/pi/sync/mutables/v3");let item=&body["items"][0];assert_ne!(item["operationId"],original_id);
            assert_eq!(item["expectedCiphertext"],current["ciphertext"]);assert_ne!(item["ciphertext"],current["ciphertext"]);assert_eq!(open(item)["atHome"],false);
            current["ciphertext"]=item["ciphertext"].clone();(200,ack(&body,"applied"))},
        _=>{assert_eq!(path,"/api/pi/sync/charge-states");assert!(body.get("since").is_none());(200,page(vec![current.clone()],2,None))},
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();assert!(!store::remaining(&state.store,&binding).unwrap());
}
#[tokio::test]
async fn mismatched_summary_identity_cannot_be_classified() {
    let (state,creds,_)=setup();let mut remote=source(1,1,None);
    remote["summaryCiphertext"]=json!(encrypt::seal_json_b64(&KEY,&aad::charge_summary("owner","pi",remote["id"].as_str().unwrap()),&json!({"id":2,"startMs":2000,"locationLat":0.0,"locationLon":0.0})).unwrap());
    let (client,task)=server(1,move|_,_,_|(200,page(vec![remote.clone()],1,None))).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();assert!(store::remaining(&state.store,&revision::binding(&creds).unwrap()).unwrap());
}

#[tokio::test]
async fn replay_receipt_cannot_restore_user_fields_changed_after_the_original_home_write() {
    let (state,creds,_)=setup();let remote=source(1,1,None);let mut later=remote.clone();
    let (client,task)=server(2,move|_,_,index|if index==0{(200,page(vec![remote.clone()],1,None))}else{(503,json!({"error":"unknown"}))}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    later["revision"]=json!("2");later["ciphertext"]=json!(encrypt::seal_json_b64(&KEY,&aad::charge_mutable("owner","pi",later["id"].as_str().unwrap()),
        &json!({"tags":["Newer"],"costOverride":{"amount":7,"currency":"USD","keep":true},"extra":"newer"})).unwrap());
    let (client,task)=server(4,move|path,body,index|match index {
        0=>(200,states(vec![later.clone()])),
        1=>{assert_eq!(path,"/api/pi/sync/mutables/v3");(200,ack(&body,"applied"))},
        2=>(200,page(vec![later.clone()],2,None)),
        _=>{let mutable=open(&body["items"][0]);assert_eq!(mutable,json!({"tags":["Newer"],"costOverride":{"amount":7,"currency":"USD","keep":true},"extra":"newer","atHome":true}));(200,ack(&body,"applied"))},
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
}
#[tokio::test]
async fn batches_are_durable_before_post_and_partial_acknowledgements_keep_all_unknown_attempts() {
    let (state,creds,_)=setup();let inspect=state.clone();let binding=revision::binding(&creds).unwrap();
    let rows=vec![source(1,1,None),source(2,2,None)];let original=rows.clone();
    let (client,task)=server(2,move|_,body,index| {
        if index==0 {return(200,page(original.clone(),2,None))}
        assert_eq!(body["items"].as_array().unwrap().len(),2);
        for item in body["items"].as_array().unwrap() {assert!(store::attempt(&inspect.store,&binding,item["id"].as_str().unwrap()).unwrap().is_some());}
        (200,json!({"ok":true,"writeProtocol":3,"results":[{"kind":"charge","id":body["items"][0]["id"],"status":"applied"}]}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();let binding=revision::binding(&creds).unwrap();
    assert_eq!(store::retries(&state.store,&binding).unwrap().len(),2);
    for row in rows {assert!(store::attempt(&state.store,&binding,row["id"].as_str().unwrap()).unwrap().is_some());}
}
#[test]
fn progress_failure_cannot_drop_a_pending_attempt_or_failed_identity() {
    let (state,creds,_)=setup();let binding=revision::binding(&creds).unwrap();let id=ids::charge_id_from_start_ts(1);
    let ready=Ready {id:id.clone(),raw:"synthetic attempt".into(),proposal:json!({}),replay:false,expected:None};
    store::reserve(&state.store,&binding,&[ready]).unwrap();
    let walk=Walk {version:1,binding:binding.clone(),policy:policy(None),since:Some("1".into()),cursor:None,full_at:1};
    let outcomes=[store::Outcome {id:id.clone(),done:true,retire:Some("synthetic attempt".into())}];
    assert!(store::finish(&state.store,&binding,&outcomes,Some((Some("different scan"),&walk)),None).is_err());
    assert!(store::attempt(&state.store,&binding,&id).unwrap().is_some());assert_eq!(store::retries(&state.store,&binding).unwrap(),vec![id]);
    assert!(store::load(&state.store,&binding).unwrap().is_none());
}
#[test]
fn failed_first_hundred_retries_do_not_starve_later_sources() {
    let (state,creds,_)=setup();let binding=revision::binding(&creds).unwrap();
    let outcomes=(1..=205).map(|id|store::Outcome {id:format!("{id:064x}"),done:false,retire:None}).collect::<Vec<_>>();
    store::finish(&state.store,&binding,&outcomes,None,None).unwrap();
    let first=store::retries(&state.store,&binding).unwrap();assert_eq!(first.len(),100);
    store::finish(&state.store,&binding,&[],None,first.last().map(String::as_str)).unwrap();
    let second=store::retries(&state.store,&binding).unwrap();assert_eq!(second.len(),100);assert_eq!(second[0],format!("{:064x}",101));
}

#[tokio::test]
async fn a_large_scan_yields_with_durable_progress_and_resumes_without_a_full_restart() {
    let (state,creds,_)=setup();let rows=(1..=4).map(|n|source(n,n,Some(true))).collect::<Vec<_>>();let initial=rows.clone();
    let (client,task)=server(3,move|path,_,index| {
        assert_eq!(path,"/api/pi/sync/charge-states");(200,page(vec![initial[index].clone()],4,Some(cursor(&initial[index],4,None))))
    }).await;
    let error=push(&state,&client,&creds,&PI_KEY).await.unwrap_err();assert!(is_pending(&error));task.await.unwrap();
    let last=rows[3].clone();let (client,task)=server(1,move|_,body,_|{assert!(body["cursor"].is_string());(200,page(vec![last.clone()],4,None))}).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    let walk:Walk=serde_json::from_str(&store::load(&state.store,&revision::binding(&creds).unwrap()).unwrap().unwrap()).unwrap();
    assert_eq!(walk.since.as_deref(),Some("4"));assert!(walk.cursor.is_none());
}

#[tokio::test]
async fn restored_cloud_clock_resets_only_scan_progress_and_restarts_a_full_walk() {
    let (state,creds,_)=setup();let binding=revision::binding(&creds).unwrap();
    let walk=Walk {version:1,binding:binding.clone(),policy:policy(state.rate_config.as_ref().unwrap().home_geofence().unwrap()),since:Some("10".into()),cursor:None,full_at:crate::state::now_ms()};
    store::finish(&state.store,&binding,&[],Some((None,&walk)),None).unwrap();
    let (client,task)=server(2,move|_,body,index| {
        if index==0 {assert_eq!(body["since"],"10");return(409,json!({"error":"sync_reset_required"}))}
        assert!(body.get("since").is_none());(200,page(vec![],1,None))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    let latest:Walk=serde_json::from_str(&store::load(&state.store,&binding).unwrap().unwrap()).unwrap();assert_eq!(latest.since.as_deref(),Some("1"));
}

#[tokio::test]
async fn a_saved_unknown_attempt_survives_database_reopen_and_a_new_state_instance() {
    let dir=tempfile::tempdir().unwrap();let db=dir.path().join("synthetic.db");
    let (mut state,creds,_)=setup();Arc::get_mut(&mut state).unwrap().store=Arc::new(crate::open_test_store(db.to_str().unwrap()).unwrap());
    let shared=Arc::new(Mutex::new(source(1,1,None)));let remote=shared.clone();
    let (client,task)=server(2,move|_,body,index| {
        if index==0 {return(200,page(vec![remote.lock().unwrap().clone()],1,None))}
        remote.lock().unwrap()["ciphertext"]=body["items"][0]["ciphertext"].clone();(503,json!({"error":"unknown"}))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();drop(state);
    let (mut reopened,creds,_)=setup();Arc::get_mut(&mut reopened).unwrap().store=Arc::new(crate::open_test_store(db.to_str().unwrap()).unwrap());
    let (client,task)=server(2,move|path,_,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/state");return(200,states(vec![shared.lock().unwrap().clone()]))}
        assert_eq!(path,"/api/pi/sync/charge-states");(200,page(vec![],2,None))
    }).await;
    push(&reopened,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(!store::remaining(&reopened.store,&revision::binding(&creds).unwrap()).unwrap());
}
#[tokio::test]
async fn changed_credentials_cannot_confirm_the_previous_pairings_attempt() {
    let (state,creds,_)=setup();let remote=source(1,1,None);let changed=state.clone();
    let (client,task)=server(2,move|_,body,index| {
        if index==0 {return(200,page(vec![remote.clone()],1,None))}
        changed.creds.try_lock().unwrap().as_mut().unwrap().pi_auth_token="replacement".into();(200,ack(&body,"applied"))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();let binding=revision::binding(&creds).unwrap();
    assert!(store::remaining(&state.store,&binding).unwrap());assert!(store::load(&state.store,&binding).unwrap().is_none());
}

//! Mirror only the Pi-owned Home setting inside the existing encrypted document.
//! No private recording, user rate preference or Cloud tag is modified here.
use super::*;
use sentryusb_drives::home::HomeGeofence;
const FIELD:&str="charging_home";
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct HomePending {version:u8,binding:String,policy:String,proposal:Value}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Checked {version:u8,policy:String,at:i64}
fn digest(value:&impl Serialize)->Result<String> {
    Ok(ring::digest::digest(&ring::digest::SHA256,&serde_json::to_vec(value)?).as_ref().iter().map(|b|format!("{b:02x}")).collect())
}
fn policy(home:Option<HomeGeofence>)->Result<Value> {
    let location=home.map(|home|->Result<Value>{
        let h=HomeGeofence::new(home.latitude,home.longitude,home.radius_m)?;
        Ok(json!({"latitude":h.latitude,"longitude":h.longitude,"radiusM":h.radius_m}))
    }).transpose()?.unwrap_or(Value::Null);
    Ok(json!({"version":1,"location":location}))
}
fn canonical_policy(value:&Value)->Result<Value> {
    let object=value.as_object().context("invalid Home setting")?;
    ensure!(object.len()==2&&object.get("version").and_then(Value::as_f64)==Some(1.0)&&object.contains_key("location"),"unsupported Home setting");
    if value["location"].is_null(){return policy(None)}
    let h=value["location"].as_object().context("invalid Home location")?;
    ensure!(h.len()==3,"unknown Home location fields");
    policy(Some(HomeGeofence::new(h.get("latitude").and_then(Value::as_f64).context("Home latitude missing")?,
        h.get("longitude").and_then(Value::as_f64).context("Home longitude missing")?,h.get("radiusM").and_then(Value::as_f64).context("Home radius missing")?)?))
}
fn same_policy(value:&Value,canonical:&Value)->bool {canonical_policy(value).is_ok_and(|value|value==*canonical)}
async fn local(state:&CloudStateInner)->Result<Value> {
    let access=state.rate_config.clone().context("Home configuration unavailable")?;
    let home=tokio::task::spawn_blocking(move||access.home_geofence()).await.context("Home configuration task")??;
    policy(home)
}
fn key(binding:&str)->String {format!("cloud_home_config_attempt_v1:{binding}")}
fn checked_key(binding:&str)->String {format!("cloud_home_config_checked_v1:{binding}")}
fn operation_id(pending:&HomePending)->Result<String> {
    let mut candidate=serde_json::to_value(pending)?;candidate["proposal"]["operationId"]=json!("");digest(&candidate)
}
fn validate(raw:&str,binding:&str,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<HomePending> {
    ensure!(raw.len()<=65536,"Home setting attempt is too large");
    let pending:HomePending=serde_json::from_str(raw)?;
    ensure!(pending.version==1&&pending.binding==binding&&hex(&pending.policy),"Home setting attempt changed");
    let mut object=pending.proposal.as_object().context("invalid Home proposal")?.clone();
    ensure!(object.remove("homeConfig")==Some(json!(true)),"Home purpose missing");
    let p:Proposal=serde_json::from_value(Value::Object(object))?;
    ensure!(p.kind=="rateConfig"&&p.id==creds.pi_id&&p.record_version.is_none()&&p.wrapped_key.is_none()
        &&p.field_merge&&(0..=8_640_000_000_000_000).contains(&p.changed_at_ms)
        &&p.operation_id==operation_id(&pending)?,"Home setting identity changed");
    revision::valid_envelope(&p.ciphertext,16384,None)?;
    if let Some(ct)=&p.expected_ciphertext {revision::valid_envelope(ct,16384,None)?;}
    let mut original=decrypt(creds,pi_key,p.expected_ciphertext.as_deref())?;
    let proposed=decrypt(creds,pi_key,Some(&p.ciphertext))?;
    let value=&proposed[FIELD];let canonical=canonical_policy(value)?;
    ensure!(digest(&canonical)?==pending.policy,"saved Home setting changed");
    original[FIELD]=value.clone();ensure!(original==proposed,"Home setting proposal changes unrelated preferences");
    Ok(pending)
}
fn finish(state:&CloudStateInner,binding:&str,raw:Option<&str>,policy:&str,checked:bool)->Result<()> {
    state.store.with_durable_conn(|conn|{
        let tx=conn.transaction()?;
        ensure!(schema::meta_get(&tx,&key(binding))?.as_deref()==raw,"Home setting attempt changed");
        if raw.is_some(){schema::meta_del(&tx,&key(binding))?;}
        if checked {schema::meta_set(&tx,&checked_key(binding),&serde_json::to_string(&Checked {version:1,policy:policy.into(),at:crate::state::now_ms()})?)?;}
        tx.commit()?;Ok(())
    })
}
async fn publish(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],binding:&str,pending:&HomePending,raw:&str)->Result<()> {
    ensure!(digest(&local(state).await?)?==pending.policy,"Home setting changed before publication");
    let status=post(state,client,creds,binding,&pending.proposal).await?;
    if status!="applied" {
        let _guard=revision::current_pairing(state,binding).await?;
        finish(state,binding,Some(raw),&pending.policy,false)?;
        anyhow::bail!("Home setting publication conflicted; refresh required");
    }
    let current=read(state,client,creds,binding).await?;
    let document=decrypt(creds,pi_key,current.ciphertext.as_deref())?;
    let matches=canonical_policy(&document[FIELD]).and_then(|value|digest(&value)).is_ok_and(|hash|hash==pending.policy);
    let _guard=revision::current_pairing(state,binding).await?;
    finish(state,binding,Some(raw),&pending.policy,matches)?;
    ensure!(matches,"Home setting changed after publication");Ok(())
}
pub(crate) async fn push(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<()> {
    if !state.rate_config.as_ref().is_some_and(|access|access.home_config_sync_enabled()){return Ok(())}
    let _run=state.home_sync.lock().await;
    let binding=revision::binding(creds)?;
    {let _guard=revision::current_pairing(state,&binding).await?;}
    let value=local(state).await?;let hash=digest(&value)?;
    let raw=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&binding)))?;
    if raw.is_none() {
        if let Some(saved)=state.store.with_locked_conn(|conn|schema::meta_get(conn,&checked_key(&binding)))? {
            let checked:Checked=serde_json::from_str(&saved)?;ensure!(checked.version==1&&hex(&checked.policy)&&checked.at>=0,"invalid Home check marker");
            let now=crate::state::now_ms();
            if checked.policy==hash&&now>=checked.at&&now-checked.at<300_000 {return Ok(())}
        }
    }
    let remote=read(state,client,creds,&binding).await?;
    // Older APIs or accounts without charging-location consent must not receive
    // this new encrypted location field. The write rechecks consent atomically.
    if remote.home_config_protocol!=Some(1)||remote.home_config_allowed!=Some(true) {
        if raw.is_none(){let _guard=revision::current_pairing(state,&binding).await?;finish(state,&binding,None,&hash,true)?;}
        return Ok(())
    }
    if remote.ciphertext.is_none() {
        // A Home-only document must not hide the ordinary uploader's missing
        // initial rates. An acknowledged empty rate setup may create the mirror.
        let initial=state.store.with_locked_conn(|conn|schema::meta_get(conn,&initial_key(&binding)))?;
        let rate_pending=state.store.dirty_mutables()?.iter().any(|(kind,id,_)|kind=="rate"&&id.is_empty());
        if initial.as_deref()!=Some("1")||rate_pending {return Ok(())}
    }
    let mut document=decrypt(creds,pi_key,remote.ciphertext.as_deref())?;
    if let Some(stored)=document.get(FIELD){canonical_policy(stored).context("unreadable or unsupported Cloud Home setting")?;}
    if let Some(saved)=raw.as_deref() {
        let pending=validate(saved,&binding,creds,pi_key)?;
        if pending.policy==hash&&!same_policy(&document[FIELD],&value)&&remote.ciphertext==pending.proposal["expectedCiphertext"].as_str().map(String::from) {
            return publish(state,client,creds,pi_key,&binding,&pending,saved).await
        }
        // A changed ciphertext cannot accept the old CAS. A matching result
        // needs no second write; a changed local policy gets a fresh proposal.
        let _guard=revision::current_pairing(state,&binding).await?;
        finish(state,&binding,Some(saved),&hash,false)?;
    }
    if same_policy(&document[FIELD],&value) {
        let _guard=revision::current_pairing(state,&binding).await?;
        return finish(state,&binding,None,&hash,true)
    }
    document[FIELD]=value;
    let ciphertext=encrypt::seal_json_b64(pi_key,&aad::rate_config(&creds.user_id,&creds.pi_id),&document)?;
    revision::valid_envelope(&ciphertext,16384,None)?;
    let mut pending=HomePending {version:1,binding:binding.clone(),policy:hash,proposal:json!({"kind":"rateConfig","id":creds.pi_id,
        "recordVersion":null,"wrappedKey":null,"expectedCiphertext":remote.ciphertext,"ciphertext":ciphertext,
        "changedAtMs":crate::state::now_ms(),"fieldMerge":true,"homeConfig":true,"operationId":""})};
    pending.proposal["operationId"]=json!(operation_id(&pending)?);
    let encoded=serde_json::to_string(&pending)?;
    {
        let _guard=revision::current_pairing(state,&binding).await?;
        state.store.with_durable_conn(|conn|{
            let tx=conn.transaction()?;ensure!(schema::meta_get(&tx,&key(&binding))?.is_none(),"Home setting already pending");
            schema::meta_set(&tx,&key(&binding),&encoded)?;tx.commit()?;Ok(())
        })?;
    }
    publish(state,client,creds,pi_key,&binding,&pending,&encoded).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use crate::state::RateConfigAccess;
    use crate::sync::{revision::tests::fixture,conditional_charge::tests::server};
    const PI_KEY:[u8;32]=[7;32];
    struct Home(Mutex<Option<HomeGeofence>>);
    impl RateConfigAccess for Home {
        fn home_config_sync_enabled(&self)->bool {true}
        fn home_geofence(&self)->Result<Option<HomeGeofence>> {Ok(*self.0.lock().unwrap())}
        fn load_doc(&self,_:&sentryusb_drives::DriveStore)->Result<Value> {anyhow::bail!("Home mirror must not read local rate intent")}
        fn store_doc(&self,_:&Value,_:&sentryusb_drives::DriveStore,_:i64)->Result<()> {anyhow::bail!("Home mirror must not change local preferences")}
    }
    fn setup()->(Arc<CloudStateInner>,CloudCredentialsV1,Arc<Home>) {
        let (mut state,creds)=fixture();let home=Arc::new(Home(Mutex::new(Some(HomeGeofence::new(0.0,0.0,120.0).unwrap()))));
        Arc::get_mut(&mut state).unwrap().rate_config=Some(home.clone());(state,creds,home)
    }
    fn remote(doc:Value)->Value {json!({"kind":"rateConfig","id":"pi","status":"ok","recordVersion":null,"wrappedKey":null,"updatedAtMs":1,
        "homeConfigProtocol":1,"homeConfigAllowed":true,"ciphertext":encrypt::seal_json_b64(&PI_KEY,&aad::rate_config("owner","pi"),&doc).unwrap()})}
    fn response(value:&Value)->Value {json!({"ok":true,"writeProtocol":3,"items":[value]})}
    fn ack()->Value {json!({"ok":true,"writeProtocol":3,"results":[{"kind":"rateConfig","id":"pi","status":"applied"}]})}
    fn opened(proposal:&Value)->Value {encrypt::open_json_b64(&PI_KEY,&aad::rate_config("owner","pi"),proposal["ciphertext"].as_str().unwrap()).unwrap()}

    #[tokio::test]
    async fn home_only_mirror_preserves_cloud_rate_fields_and_is_durable_before_post() {
        let (state,creds,_)=setup();let inspect=state.clone();let binding=revision::binding(&creds).unwrap();let saved_binding=binding.clone();
        let before=json!({"charging_currency":"CAD","charging_tag_rates":{"Home":0.12},"extension":{"preserve":true}});let mut current=remote(before.clone());
        let (client,task)=server(3,move|path,body,index|{
            if index==1 {
                assert_eq!(path,"/api/pi/sync/mutables/v3");let proposed=&body["items"][0];assert_eq!(proposed["homeConfig"],true);
                assert_eq!(proposed["expectedCiphertext"],current["ciphertext"]);
                let mut expected=before.clone();expected[FIELD]=policy(Some(HomeGeofence::new(0.0,0.0,120.0).unwrap())).unwrap();
                assert_eq!(opened(proposed),expected);
                let pending=inspect.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&saved_binding))).unwrap().unwrap();
                let pending:HomePending=serde_json::from_str(&pending).unwrap();assert_eq!(pending.proposal,*proposed);assert!(hex(&pending.policy));
                current["ciphertext"]=proposed["ciphertext"].clone();return (200,ack())
            }
            (200,response(&current))
        }).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
        assert!(state.store.dirty_mutables().unwrap().is_empty());
        assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&binding))).unwrap().is_none());
        // Confirmation avoids another network read while policy is unchanged.
        push(&state,&client,&creds,&PI_KEY).await.unwrap();
    }
    #[tokio::test]
    async fn unknown_commit_is_read_back_without_a_second_publication() {
        let (state,creds,_)=setup();let shared=Arc::new(Mutex::new(remote(json!({"charging_default_rate":0.2}))));let published=shared.clone();
        let (client,task)=server(2,move|_,body,index|{
            if index==0{return (200,response(&published.lock().unwrap()))}
            published.lock().unwrap()["ciphertext"]=body["items"][0]["ciphertext"].clone();(503,json!({"error":"unknown"}))
        }).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        let (client,task)=server(1,move|path,_,_|{assert_eq!(path,"/api/pi/sync/state");(200,response(&shared.lock().unwrap()))}).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    }
    #[tokio::test]
    async fn old_api_or_missing_consent_never_receives_location_ciphertext() {
        for supported in [false,true] {
            let (state,creds,_)=setup();let mut current=remote(json!({}));
            if supported {current["homeConfigAllowed"]=json!(false)}else{current.as_object_mut().unwrap().remove("homeConfigProtocol");}
            let (client,task)=server(1,move|path,_,_|{assert_eq!(path,"/api/pi/sync/state");(200,response(&current))}).await;
            push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
            let binding=revision::binding(&creds).unwrap();assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&binding))).unwrap().is_none());
        }
    }
    #[tokio::test]
    async fn changed_local_home_replaces_pending_intent_without_replaying_old_settings() {
        let (state,creds,home)=setup();let original=remote(json!({"charging_default_rate":0.1}));
        let (client,task)=server(2,move|_,_,i|if i==0{(200,response(&original))}else{(503,json!({"error":"unknown"}))}).await;
        assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        let binding=revision::binding(&creds).unwrap();let raw=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&binding))).unwrap().unwrap();
        let old:HomePending=serde_json::from_str(&raw).unwrap();*home.0.lock().unwrap()=None;
        let mut current=remote(json!({"charging_default_rate":0.3,"keep":"newer"}));
        let (client,task)=server(3,move|_,body,index|{
            if index==1 {
                let proposed=&body["items"][0];assert_ne!(proposed["operationId"],old.proposal["operationId"]);
                assert_eq!(proposed["expectedCiphertext"],current["ciphertext"]);
                assert_eq!(opened(proposed),json!({"charging_default_rate":0.3,"keep":"newer","charging_home":{"version":1,"location":null}}));
                current["ciphertext"]=proposed["ciphertext"].clone();return (200,ack())
            }
            (200,response(&current))
        }).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    }
    #[tokio::test]
    async fn home_mirror_cannot_hide_unfinished_initial_rate_publication() {
        let (state,creds,_)=setup();let mut current=remote(json!({}));current["ciphertext"]=Value::Null;
        let (client,task)=server(1,move|path,_,_|{assert_eq!(path,"/api/pi/sync/state");(200,response(&current))}).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
        let binding=revision::binding(&creds).unwrap();assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&key(&binding))).unwrap().is_none());
    }

    #[tokio::test]
    async fn browser_json_integer_roundtrip_does_not_trigger_another_home_write() {
        let (state,creds,_)=setup();
        let current=remote(json!({"charging_home":{"version":1,"location":{"latitude":0,"longitude":0,"radiusM":120}},"charging_default_rate":0.2}));
        let (client,task)=server(1,move|path,_,_|{assert_eq!(path,"/api/pi/sync/state");(200,response(&current))}).await;
        push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    }

    #[tokio::test]
    async fn unknown_home_versions_and_fields_are_not_silently_overwritten() {
        for home in [json!({"version":2,"location":null}),json!({"version":1,"location":null,"future":true})] {
            let (state,creds,_)=setup();let current=remote(json!({"charging_home":home}));
            let (client,task)=server(1,move|path,_,_|{assert_eq!(path,"/api/pi/sync/state");(200,response(&current))}).await;
            assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        }
    }

    #[test]
    fn native_classification_matches_shared_browser_policy_vectors() {
        let vectors:Value=serde_json::from_str(include_str!("../../../test-support/chargingHomePolicyV1.json")).unwrap();
        for vector in vectors.as_array().unwrap() {
            let value=canonical_policy(&vector["policy"]).unwrap();let h=&value["location"];
            let home=if h.is_null(){None}else{Some(HomeGeofence::new(h["latitude"].as_f64().unwrap(),h["longitude"].as_f64().unwrap(),h["radiusM"].as_f64().unwrap()).unwrap())};
            for point in vector["points"].as_array().unwrap() {
                assert_eq!(home.is_some_and(|home|home.contains(point[0].as_f64(),point[1].as_f64())),point[2].as_bool().unwrap(),"{}",vector["name"]);
            }
        }
    }

}

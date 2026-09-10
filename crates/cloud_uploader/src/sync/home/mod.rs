//! Background Home metadata uses authenticated Cloud summaries, not restored
//! local telemetry assumed to belong to the original upload.
use std::{collections::BTreeSet,sync::Arc};
use anyhow::{Context,Result,ensure};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use sentryusb_cloud_crypto::{aad,credentials::CloudCredentialsV1,ids};
use sentryusb_drives::home::HomeGeofence;
use crate::{client::CloudClient,encrypt,state::CloudStateInner};
use super::revision;
mod wire;
mod store;
const PAGES_PER_PASS:usize=3;
#[derive(Debug,thiserror::Error)]
#[error("Home charging update will continue")]
struct MoreWork;
#[derive(Debug,thiserror::Error)]
#[error("Home scan checkpoint must be refreshed")]
struct Reset;
pub(super) fn is_pending(error:&anyhow::Error)->bool {error.is::<MoreWork>()}

#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Walk {version:u8,binding:String,policy:String,since:Option<String>,cursor:Option<String>,full_at:i64}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {version:u8,binding:String,policy:String,proposal:Value}
struct Source {id:String,version:String,wrapped:String,summary:String,ciphertext:Option<String>,key:[u8;32],document:Value,at_home:bool}
fn digest(bytes:&[u8])->String {ring::digest::digest(&ring::digest::SHA256,bytes).as_ref().iter().map(|byte|format!("{byte:02x}")).collect()}
fn policy(home:Option<HomeGeofence>)->String {
    let mut bytes=b"sentrycloud-home-policy-v1".to_vec();
    if let Some(home)=home {bytes.push(1);for value in [home.latitude,home.longitude,home.radius_m] {bytes.extend_from_slice(&value.to_be_bytes());}}
    else {bytes.push(0)}
    digest(&bytes)
}
async fn current_home(state:&CloudStateInner)->Result<Option<HomeGeofence>> {
    let access=state.rate_config.clone().context("Home configuration unavailable")?;
    tokio::task::spawn_blocking(move||access.home_geofence()).await.context("Home config task")?
}
fn source(value:&Value,creds:&CloudCredentialsV1,pi_key:&[u8;32],home:Option<HomeGeofence>)->Result<Source> {
    let id=wire::id(value)?.to_string();ensure!(value["status"]=="ok","Home source is missing");
    let version=value["recordVersion"].as_str().filter(|value|wire::hex(value)).context("invalid Home source version")?.to_string();
    let wrapped=value["wrappedKey"].as_str().context("missing Home source key")?.to_string();revision::valid_envelope(&wrapped,61,Some(61))?;
    let summary=value["summaryCiphertext"].as_str().context("missing Home source summary")?.to_string();revision::valid_envelope(&summary,4096,None)?;
    let ciphertext=match value.get("ciphertext") {Some(Value::Null)=>None,Some(Value::String(ct))=>{revision::valid_envelope(ct,2048,None)?;Some(ct.clone())},_=>anyhow::bail!("missing Home mutable state")};
    let key=encrypt::unwrap_content_key(pi_key,&wrapped,&aad::charge_key(&creds.user_id,&creds.pi_id,&id))?;
    let decoded:Value=encrypt::open_json_b64(&key,&aad::charge_summary(&creds.user_id,&creds.pi_id,&id),&summary)?;
    let ts=decoded["id"].as_i64().context("invalid Home summary identity")?;
    ensure!(ids::charge_id_from_start_ts(ts)==id && decoded["startMs"].as_i64()==ts.checked_mul(1000),"Home summary does not match source");
    let coordinate=|field:&str|->Result<Option<f64>> {match decoded.get(field) {
        Some(Value::Null)=>Ok(None),Some(Value::Number(number))=>Ok(Some(number.as_f64().filter(|value|value.is_finite()).context("invalid Home coordinate")?)),
        _=>anyhow::bail!("unreadable Home coordinate"),
    }};
    let lat=coordinate("locationLat")?;let lon=coordinate("locationLon")?;
    ensure!(lat.is_none_or(|value|(-90.0..=90.0).contains(&value)),"invalid Home latitude");
    let at_home=home.is_some_and(|home|home.contains(lat,lon));
    let document:Value=match &ciphertext {Some(ct)=>encrypt::open_json_b64(&key,&aad::charge_mutable(&creds.user_id,&creds.pi_id,&id),ct)?,None=>json!({"tags":[],"costOverride":null})};
    ensure!(document.is_object() && document.get("atHome").is_none_or(|value|value.is_null()||value.is_boolean()),"unreadable Home classification");
    Ok(Source {id,version,wrapped,summary,ciphertext,key,document,at_home})
}
fn operation_id(binding:&str,policy:&str,proposal:&Value)->Result<String> {
    let mut proposal=proposal.clone();proposal.as_object_mut().context("invalid Home proposal")?.remove("operationId");
    Ok(digest(&serde_json::to_vec(&(binding,policy,proposal))?))
}
fn saved(raw:&str,binding:&str,id:&str,creds:&CloudCredentialsV1,pi_key:&[u8;32],home:Option<HomeGeofence>)->Result<Pending> {
    let pending:Pending=serde_json::from_str(raw)?;let p=&pending.proposal;
    ensure!(pending.version==1 && pending.binding==binding && wire::hex(&pending.policy) && p["id"]==id && p["kind"]=="charge"
        && p["fieldMerge"]==true && p["operationId"].as_str()==Some(operation_id(binding,&pending.policy,p)?.as_str())
        && p["changedAtMs"].as_i64().is_some_and(|at|at>=0),"Home attempt identity changed");
    let original=source(&json!({"kind":"charge","id":id,"status":"ok","recordVersion":p["recordVersion"],"wrappedKey":p["wrappedKey"],
        "summaryCiphertext":p["expectedSummaryCiphertext"],"ciphertext":p["expectedCiphertext"]}),creds,pi_key,home)?;
    let ciphertext=p["ciphertext"].as_str().context("missing saved Home ciphertext")?;revision::valid_envelope(ciphertext,2048,None)?;
    let doc:Value=encrypt::open_json_b64(&original.key,&aad::charge_mutable(&creds.user_id,&creds.pi_id,id),ciphertext)?;
    ensure!(doc.is_object() && doc["atHome"].is_boolean(),"invalid saved Home classification");
    if pending.policy==policy(home) {ensure!(doc["atHome"]==original.at_home,"saved Home policy disagrees");}
    // Only Home may differ from the original document; a stored attempt must
    // never carry an accidental rewrite of user tags/costs/extensions.
    let mut expected=original.document;expected["atHome"]=json!(doc["atHome"]);ensure!(doc==expected,"saved Home attempt changes unrelated fields");
    Ok(pending)
}
struct Ready {id:String,raw:String,proposal:Value,replay:bool,expected:Option<String>}
async fn process(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],binding:&str,home:Option<HomeGeofence>,items:&[Value])->Result<Vec<store::Outcome>> {
    let hash=policy(home);let mut outcomes=Vec::new();let mut ready=Vec::new();
    let preparation_guard=revision::current_pairing(state,binding).await?;
    for item in items {
        let id=wire::id(item)?.to_owned();let old=store::attempt(&state.store,binding,&id)?;
        if item["status"]=="not_found" {outcomes.push(store::Outcome {id,done:true,retire:old});continue}
        let prepared:Result<Option<Ready>>=(|| {
            let source=source(item,creds,pi_key,home)?;
            if let Some(raw)=&old {
                let previous=saved(raw,binding,&id,creds,pi_key,home)?;
                if previous.policy==hash {
                    let p=&previous.proposal;
                    if p["recordVersion"]==source.version && p["wrappedKey"]==source.wrapped && p["expectedSummaryCiphertext"]==source.summary
                        && source.ciphertext.as_deref()==p["ciphertext"].as_str() {return Ok(None)}
                    return Ok(Some(Ready {id:id.clone(),raw:raw.clone(),proposal:previous.proposal,replay:true,expected:Some(raw.clone())}))
                }
            } else if source.document["atHome"]==source.at_home {return Ok(None)}
            // A changed policy always publishes a fresh nonce, even if the
            // current boolean already matches. It invalidates an older
            // unknown request's expected ciphertext without replaying it.
            let mut document=source.document;document["atHome"]=json!(source.at_home);
            let ciphertext=encrypt::seal_json_b64(&source.key,&aad::charge_mutable(&creds.user_id,&creds.pi_id,&source.id),&document)?;
            revision::valid_envelope(&ciphertext,2048,None)?;
            let mut proposal=json!({"kind":"charge","id":id,"recordVersion":source.version,"wrappedKey":source.wrapped,
                "expectedSummaryCiphertext":source.summary,"expectedCiphertext":source.ciphertext,"ciphertext":ciphertext,"fieldMerge":true,"changedAtMs":crate::state::now_ms()});
            proposal["operationId"]=json!(operation_id(binding,&hash,&proposal)?);
            let raw=serde_json::to_string(&Pending {version:1,binding:binding.into(),policy:hash.clone(),proposal:proposal.clone()})?;
            Ok(Some(Ready {id:id.clone(),raw,proposal,replay:false,expected:old.clone()}))
        })();
        match prepared {Ok(Some(value))=>ready.push(value),Ok(None)=>outcomes.push(store::Outcome {id,done:true,retire:old}),Err(_)=>outcomes.push(store::Outcome {id,done:false,retire:None})}
    }
    if !ready.is_empty() {store::reserve(&state.store,binding,&ready)?;}
    drop(preparation_guard);
    if ready.is_empty() {return Ok(outcomes)}
    ensure!(current_home(state).await?==home,"Home changed before publication");
    let response=wire::request(state,client,creds,binding,"/api/pi/sync/mutables/v3",json!({"piId":creds.pi_id,
        "dekRotationGeneration":creds.dek_rotation_generation,"items":ready.iter().map(|item|&item.proposal).collect::<Vec<_>>()})).await;
    let statuses:Result<Vec<(String,String)>>=(|| {
        let response=response?;let rows=response["results"].as_array().context("missing Home acknowledgements")?;
        let wanted:BTreeSet<_>=ready.iter().map(|item|item.id.as_str()).collect();let mut seen=BTreeSet::new();
        ensure!(rows.len()==ready.len(),"incomplete Home acknowledgements");
        rows.iter().map(|row| {let id=wire::id(row)?;let status=row["status"].as_str().context("invalid Home acknowledgement")?;
            ensure!(wanted.contains(id)&&seen.insert(id)&&matches!(status,"applied"|"conflict"|"source_changed"|"not_found"),"unexpected Home acknowledgement");Ok((id.into(),status.into()))}).collect()
    })();
    let same_policy=current_home(state).await.is_ok_and(|current|current==home);
    for item in ready {
        if let Ok(statuses)=&statuses {
            let status=&statuses.iter().find(|(id,_)|id==&item.id).unwrap().1;
            // A replay receipt can precede another edit. Retain a fresh read
            // request even after confirmation; never write its old plaintext.
            outcomes.push(store::Outcome {id:item.id,done:status=="applied"&&!item.replay&&same_policy,retire:Some(item.raw)});
        } else {outcomes.push(store::Outcome {id:item.id,done:false,retire:None});}
    }
    Ok(outcomes)
}

pub(super) async fn push(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<()> {
    if state.rate_config.is_none() {return Ok(())}
    let _run=state.home_sync.lock().await;
    let binding=revision::binding(creds)?;{let _guard=revision::current_pairing(state,&binding).await?;}
    let home=current_home(state).await?;let hash=policy(home);
    let retry=store::retries(&state.store,&binding)?;
    if !retry.is_empty() {
        let items=wire::targets(state,client,creds,&binding,&retry).await?;
        let outcomes=process(state,client,creds,pi_key,&binding,home,&items).await?;
        let _guard=revision::current_pairing(state,&binding).await?;
        store::finish(&state.store,&binding,&outcomes,None,retry.last().map(String::as_str))?;
    }
    let mut raw=store::load(&state.store,&binding)?;
    let now=crate::state::now_ms();
    let mut walk=match &raw {Some(raw)=>serde_json::from_str::<Walk>(raw).context("invalid Home scan progress")?,None=>Walk {version:1,binding:binding.clone(),policy:hash.clone(),since:None,cursor:None,full_at:0}};
    ensure!(walk.version==1 && walk.binding==binding,"Home scan pairing changed");
    if walk.policy!=hash || (walk.cursor.is_none() && (now.saturating_sub(walk.full_at)>=86_400_000 || walk.full_at>now)) {
        walk=Walk {version:1,binding:binding.clone(),policy:hash,since:None,cursor:None,full_at:0};
    }
    let mut pages=0;let mut reset=false;
    loop {
        ensure!(current_home(state).await?==home,"Home changed during scan");
        let page=match wire::page(state,client,creds,&binding,&walk).await {
            Ok(page)=>page,
            Err(error) if error.is::<Reset>() && !reset=>{
                reset=true;
                walk=Walk {version:1,binding:binding.clone(),policy:policy(home),since:None,cursor:None,full_at:0};
                let _guard=revision::current_pairing(state,&binding).await?;
                store::finish(&state.store,&binding,&[],Some((raw.as_deref(),&walk)),None)?;
                raw=Some(serde_json::to_string(&walk)?);
                continue;
            }
            Err(error)=>return Err(error),
        };
        let outcomes=process(state,client,creds,pi_key,&binding,home,&page.items).await?;
        let done=page.next.is_none();walk.cursor=page.next;
        if done {if walk.since.is_none(){walk.full_at=crate::state::now_ms();}walk.since=Some(page.revision);}
        {
            let _guard=revision::current_pairing(state,&binding).await?;
            store::finish(&state.store,&binding,&outcomes,Some((raw.as_deref(),&walk)),None)?;
        }
        raw=Some(serde_json::to_string(&walk)?);
        if done {break}
        pages+=1;
        if pages>=PAGES_PER_PASS {
            ensure!(!store::remaining(&state.store,&binding)?,"some Home classifications need another read");
            return Err(MoreWork.into());
        }
    }
    ensure!(!store::remaining(&state.store,&binding)?,"some Home classifications need another read");Ok(())
}

#[cfg(test)]
mod tests;

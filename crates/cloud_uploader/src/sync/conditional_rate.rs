//! Rate publication retains exact encrypted requests across uncertain outcomes.
use std::sync::Arc;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sentryusb_cloud_crypto::{aad, credentials::CloudCredentialsV1};
use sentryusb_drives::{rate_intent, schema};
use crate::{client::CloudClient, encrypt, state::CloudStateInner};
use super::revision;

fn nullable<'de,D,T>(deserializer:D)->std::result::Result<Option<T>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> {Option::<T>::deserialize(deserializer)}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Proposal {
    kind:String, id:String,
    #[serde(deserialize_with="nullable")]
    record_version:Option<String>,
    #[serde(deserialize_with="nullable")]
    wrapped_key:Option<String>,
    #[serde(deserialize_with="nullable")]
    expected_ciphertext:Option<String>,
    ciphertext:String, changed_at_ms:i64, field_merge:bool, operation_id:String,
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {version:u8,binding:String,proposal:Proposal}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Remote {
    status:String,kind:String,id:String,
    #[serde(deserialize_with="nullable")]
    record_version:Option<String>,
    #[serde(deserialize_with="nullable")]
    wrapped_key:Option<String>,
    #[serde(deserialize_with="nullable")]
    ciphertext:Option<String>,updated_at_ms:i64,
    home_config_protocol:Option<u8>,home_config_allowed:Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct States {ok:bool,write_protocol:u8,items:Vec<Remote>}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Acks {ok:bool,write_protocol:u8,results:Vec<Ack>}
#[derive(Deserialize)]
struct Ack {kind:String,id:String,status:String}
fn hex(value:&str)->bool {value.len()==64 && value.bytes().all(|byte|byte.is_ascii_digit()||(b'a'..=b'f').contains(&byte))}
fn receipt_key(binding:&str)->String {format!("cloud_rate_publication_v3:{binding}")}
fn initial_key(binding:&str)->String {format!("cloud_rate_initial_checked_v1:{binding}")}
fn decrypt(creds:&CloudCredentialsV1,pi_key:&[u8;32],ciphertext:Option<&str>)->Result<Value> {
    let doc=match ciphertext {
        Some(ciphertext)=>encrypt::open_json_b64(pi_key,&aad::rate_config(&creds.user_id,&creds.pi_id),ciphertext)?,
        None=>json!({}),
    };
    ensure!(doc.is_object(),"Cloud rates are not an object");Ok(doc)
}
async fn read(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str)->Result<Remote> {
    {let _guard=revision::current_pairing(state,binding).await?;}
    let response=client.post_json_bearer("/api/pi/sync/state",&json!({"piId":creds.pi_id,
        "dekRotationGeneration":creds.dek_rotation_generation,"items":[{"kind":"rateConfig","id":creds.pi_id}]})).await?;
    ensure!(response.status().is_success(),"Cloud rate state read failed");
    let mut parsed:States=serde_json::from_slice(&revision::bounded_body(response).await?)?;
    ensure!(parsed.ok && parsed.write_protocol==3 && parsed.items.len()==1,"incomplete Cloud rate state");
    let remote=parsed.items.pop().unwrap();
    ensure!(remote.status=="ok" && remote.kind=="rateConfig" && remote.id==creds.pi_id
        && remote.record_version.is_none() && remote.wrapped_key.is_none()
        && (0..=8_640_000_000_000_000).contains(&remote.updated_at_ms),"unexpected Cloud rate source");
    if let Some(ciphertext)=&remote.ciphertext {revision::valid_envelope(ciphertext,16384,None)?;}
    {let _guard=revision::current_pairing(state,binding).await?;}
    Ok(remote)
}
async fn post(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str,proposal:&impl Serialize)->Result<String> {
    {let _guard=revision::current_pairing(state,binding).await?;}
    let response=client.post_json_bearer("/api/pi/sync/mutables/v3",&json!({"piId":creds.pi_id,
        "dekRotationGeneration":creds.dek_rotation_generation,"items":[proposal]})).await?;
    ensure!(response.status().is_success(),"Cloud rate save is unconfirmed");
    let parsed:Acks=serde_json::from_slice(&revision::bounded_body(response).await?)?;
    ensure!(parsed.ok && parsed.write_protocol==3 && parsed.results.len()==1,"incomplete rate acknowledgement");
    let ack=&parsed.results[0];
    ensure!(ack.kind=="rateConfig" && ack.id==creds.pi_id
        && matches!(ack.status.as_str(),"applied"|"conflict"|"source_changed"|"not_found"),"invalid rate acknowledgement");
    {let _guard=revision::current_pairing(state,binding).await?;}
    Ok(ack.status.clone())
}
fn retire_attempt(state:&CloudStateInner,key:&str,raw:&str)->Result<()> {
    state.store.with_durable_conn(|conn| {
        let tx=conn.transaction()?;
        ensure!(schema::meta_get(&tx,key)?.as_deref()==Some(raw),"rate publication changed");
        schema::meta_del(&tx,key)?;tx.commit()?;Ok(())
    })
}
async fn confirm_current(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
    binding:&str,pending:&Pending,key:&str,raw:&str)->Result<()> {
    let current=read(state,client,creds,binding).await?;
    let doc=decrypt(creds,pi_key,current.ciphertext.as_deref())?;
    let _guard=revision::current_pairing(state,binding).await?;
    state.rate_config.as_ref().context("rate preferences unavailable")?
        .confirm_doc(&doc,&state.store,pending.proposal.changed_at_ms,Some((key,raw)))
}
async fn recover(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
    binding:&str,key:&str,raw:&str)->Result<()> {
    let pending:Pending=serde_json::from_str(raw).context("invalid pending rate publication")?;
    ensure!(pending.version==3 && pending.binding==binding && key==receipt_key(binding)
        && pending.proposal.kind=="rateConfig" && pending.proposal.id==creds.pi_id && pending.proposal.field_merge
        && pending.proposal.record_version.is_none() && pending.proposal.wrapped_key.is_none()
        && hex(&pending.proposal.operation_id) && pending.proposal.changed_at_ms>0,"rate publication identity changed");
    revision::valid_envelope(&pending.proposal.ciphertext,16384,None)?;
    if let Some(ciphertext)=&pending.proposal.expected_ciphertext {revision::valid_envelope(ciphertext,16384,None)?;}
    // Authenticate the saved proposal even if its receipt was already committed.
    decrypt(creds,pi_key,Some(&pending.proposal.ciphertext))?;
    let current=read(state,client,creds,binding).await?;
    if current.ciphertext.as_deref()==Some(&pending.proposal.ciphertext) {
        let doc=decrypt(creds,pi_key,current.ciphertext.as_deref())?;
        let _guard=revision::current_pairing(state,binding).await?;
        return state.rate_config.as_ref().context("rate preferences unavailable")?
            .confirm_doc(&doc,&state.store,pending.proposal.changed_at_ms,Some((key,raw)));
    }
    if post(state,client,creds,binding,&pending.proposal).await? != "applied" {
        let _guard=revision::current_pairing(state,binding).await?;
        retire_attempt(state,key,raw)?;
        anyhow::bail!("rate publication was rejected; a fresh merge is required");
    }
    // A receipt may precede another Cloud edit. Never adopt the stale proposal.
    confirm_current(state,client,creds,pi_key,binding,&pending,key,raw).await
}

async fn prepare_initial(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,
    pi_key:&[u8;32],binding:&str)->Result<()> {
    if state.store.dirty_mutables()?.iter().any(|(kind,key,_)|kind=="rate" && key.is_empty()) {return Ok(())}
    let Some(access)=state.rate_config.as_ref() else {return Ok(())};
    let key=initial_key(binding);
    if let Some(marker)=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key))? {
        ensure!(marker=="1","invalid initial rate checkpoint");return Ok(())
    }
    ensure!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(binding)))?.is_none(),
        "pending rate publication has no local queue; reconciliation required");
    let current=read(state,client,creds,binding).await?;
    decrypt(creds,pi_key,current.ciphertext.as_deref())?;
    let _guard=revision::current_pairing(state,binding).await?;
    if current.ciphertext.is_none() {access.queue_initial_doc(&state.store)?;}
    // Queue reservation precedes this checkpoint. A restart between them sees
    // the durable edit; a later pairing has its own credential-bound checkpoint.
    state.store.with_durable_conn(|conn|schema::meta_set(conn,&key,"1"))?;
    Ok(())
}

pub(super) async fn push(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<()> {
    let binding=revision::binding(creds)?;
    {let _guard=revision::current_pairing(state,&binding).await?;}
    prepare_initial(state,client,creds,pi_key,&binding).await?;
    let Some((_,_,through))=state.store.dirty_mutables()?.into_iter().find(|(kind,key,_)|kind=="rate" && key.is_empty()) else {return Ok(())};
    let access=state.rate_config.as_ref().context("rate preferences unavailable")?;
    // This read completes any interrupted local edit/confirmation first.
    let local=access.load_doc(&state.store)?;
    if !state.store.dirty_mutables()?.iter().any(|(kind,key,at)|kind=="rate" && key.is_empty() && *at==through) {return Ok(())}
    let snapshot=state.store.rate_sync_snapshot()?;
    ensure!(snapshot.generation==through,"rate edit changed during snapshot");
    let key=receipt_key(&binding);
    if let Some(raw)=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key))? {
        return recover(state,client,creds,pi_key,&binding,&key,&raw).await;
    }
    let remote=read(state,client,creds,&binding).await?;
    let cloud=decrypt(creds,pi_key,remote.ciphertext.as_deref())?;
    let merged=if remote.ciphertext.is_none() {
        // There is no Cloud baseline for edits made before the first upload.
        // Publish the selected local snapshot, guarded by expectedCiphertext=null.
        // An encrypted empty object is an existing document, not this case.
        rate_intent::initial_document(&local)?
    } else if snapshot.intent.legacy {
        let selected:serde_json::Map<_,_>=sentryusb_drives::mutable_intent::RATE_KEYS.iter()
            .filter_map(|key|cloud.get(*key).map(|value|((*key).into(),value.clone()))).collect();
        ensure!(local==Value::Object(selected),"older rate edits need reconciliation");cloud.clone()
    } else {rate_intent::merge(&snapshot.intent,&cloud)?};
    if merged==cloud {
        let _guard=revision::current_pairing(state,&binding).await?;
        return access.confirm_doc(&cloud,&state.store,through,None);
    }
    let ciphertext=encrypt::seal_json_b64(pi_key,&aad::rate_config(&creds.user_id,&creds.pi_id),&merged)?;
    revision::valid_envelope(&ciphertext,16384,None)?;
    let mut pending=Pending {version:3,binding:binding.clone(),proposal:Proposal {kind:"rateConfig".into(),id:creds.pi_id.clone(),
        record_version:None,wrapped_key:None,expected_ciphertext:remote.ciphertext,ciphertext,changed_at_ms:through,field_merge:true,operation_id:String::new()}};
    pending.proposal.operation_id=ring::digest::digest(&ring::digest::SHA256,&serde_json::to_vec(&pending)?)
        .as_ref().iter().map(|byte|format!("{byte:02x}")).collect();
    let raw=serde_json::to_string(&pending)?;
    {
        let _guard=revision::current_pairing(state,&binding).await?;
        state.store.with_durable_conn(|conn| {
            let tx=conn.transaction()?;
            ensure!(schema::meta_get(&tx,&key)?.is_none(),"rate publication already pending");
            let generation:i64=tx.query_row("SELECT changed_at FROM mutable_dirty WHERE kind='rate' AND key=''",[],|row|row.get(0))?;
            ensure!(generation==through && sentryusb_drives::mutable_intent::read(&tx,"rate","")?==snapshot.intent,
                "rate intent changed before publication");
            schema::meta_set(&tx,&key,&raw)?;tx.commit()?;Ok(())
        })?;
    }
    match post(state,client,creds,&binding,&pending.proposal).await {
        Ok(status) if status=="applied"=>confirm_current(state,client,creds,pi_key,&binding,&pending,&key,&raw).await,
        Ok(_)=>{
            let _guard=revision::current_pairing(state,&binding).await?;
            retire_attempt(state,&key,&raw)?;anyhow::bail!("rate publication conflicted; edits remain queued")
        }
        Err(_)=>recover(state,client,creds,pi_key,&binding,&key,&raw).await,
    }
}

#[cfg(test)]
mod tests;

mod home_config;
pub(super) use home_config::push as push_home_config;

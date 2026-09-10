use std::collections::{HashMap,HashSet};
use std::sync::Arc;
use anyhow::{Context as _,Result,ensure};
use serde_json::{Value,json};
use sentryusb_cloud_crypto::credentials::CloudCredentialsV1;
use crate::{client::CloudClient,state::CloudStateInner};
use super::{revision::{self,Item,Kind},incoming_retry::{self,Target},route_pull,RouteChange};

pub(super) struct Context {
    pub(super) progress:(Option<String>,Option<String>),
    pub(super) retry_generation:Option<i64>,
}
async fn guard<'a>(state:&'a CloudStateInner,binding:&str,context:&Context)
    ->Result<tokio::sync::MutexGuard<'a,Option<CloudCredentialsV1>>> {
    let guard=revision::current_pairing(state,binding).await?;
    revision::check_progress(state,&context.progress.0,&context.progress.1)?;
    if let Some(expected)=context.retry_generation {
        ensure!(state.store.with_locked_conn(|conn|incoming_retry::generation(conn,binding))?==expected,
            "incoming retry queue changed before application");
    }
    Ok(guard)
}
fn target(item:&Item)->Target {Target {kind:item.kind,id:item.id.clone()}}
fn unavailable(error:&anyhow::Error)->bool {
    error.chain().any(|cause|cause.is::<reqwest::Error>() || cause.is::<route_pull::ReadUnavailable>())
}

pub(super) async fn apply(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
    items:Vec<Item>,context:&Context)->Result<(Vec<Target>,Vec<Target>)> {
    let binding=revision::binding(creds)?;let mut done=Vec::new();let mut failed=Vec::new();let mut routes=Vec::new();
    for item in items {
        if item.kind==Kind::Route {routes.push(target(&item));continue}
        let target=target(&item);
        let _guard=guard(state,&binding,context).await?;
        match super::apply_change_page(state,&creds.user_id,&creds.pi_id,pi_key,&revision::changes(vec![item])) {
            Ok(())=>done.push(target),Err(_)=>failed.push(target),
        }
    }
    let mut groups=if routes.is_empty(){Vec::new()}else{vec![routes]};
    while let Some(group)=groups.pop() {
        let changes=group.iter().map(|target|RouteChange {route_id:target.id.clone()}).collect::<Vec<_>>();
        let prepared=route_pull::prepare(state,client,creds,pi_key,&changes).await;
        let _guard=guard(state,&binding,context).await?;
        match prepared {
            Ok(Some(projection))=>match projection.apply(state) {Ok(())=>done.extend(group),Err(_)=>failed.extend(group)},
            Ok(None)=>done.extend(group),
            Err(error) if group.len()>1 && !unavailable(&error)=>{
                // Healthy pages use one complete projection. Bisect only when
                // source/decryption failures need isolating, never on HTTP failure.
                let split=group.len()/2;groups.push(group[split..].to_vec());groups.push(group[..split].to_vec());
            }
            Err(_)=>failed.extend(group),
        }
    }
    Ok((done,failed))
}

async fn read_states(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,targets:&[Target])
    ->Result<HashMap<String,Value>> {
    if targets.is_empty() {return Ok(HashMap::new())}
    let binding=revision::binding(creds)?;
    {let _guard=revision::current_pairing(state,&binding).await?;}
    let response=client.post_json_bearer("/api/pi/sync/state",&json!({
        "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,
        "items":targets.iter().map(|target|json!({"kind":target.kind.name(),"id":target.id})).collect::<Vec<_>>()
    })).await.context("read pending incoming state")?;
    ensure!(response.status().is_success(),"pending incoming state unavailable");
    let body:Value=serde_json::from_slice(&revision::bounded_body(response).await?)?;
    let rows=body["items"].as_array().context("missing pending incoming states")?;
    ensure!(body["ok"]==true && body["writeProtocol"]==3 && rows.len()==targets.len(),"incomplete pending incoming states");
    let wanted:HashSet<_>=targets.iter().map(|target|format!("{}:{}",target.kind.name(),target.id)).collect();
    let mut result=HashMap::new();
    for row in rows {
        let id=row["id"].as_str().context("missing pending record id")?;
        let kind=row["kind"].as_str().context("missing pending record kind")?;
        let key=format!("{kind}:{id}");ensure!(wanted.contains(&key) && !result.contains_key(&key),"unexpected pending incoming record");
        result.insert(key,row.clone());
    }
    Ok(result)
}
fn current_item(target:&Target,row:&Value,creds:&CloudCredentialsV1)->Result<Option<Item>> {
    ensure!(row["kind"]==target.kind.name() && row["id"]==target.id,"pending incoming identity changed");
    if row["status"]=="not_found" && target.kind==Kind::Charge {
        // A Cloud deletion does not delete the Pi's source or tags.
        return Ok(None)
    }
    ensure!(row["status"]=="ok","pending incoming source unavailable");
    let at=row["updatedAtMs"].as_i64().context("missing pending timestamp")?;
    ensure!((0..=8_640_000_000_000_000).contains(&at),"invalid pending timestamp");
    let ciphertext=match row.get("ciphertext") {
        Some(Value::Null)=>None,Some(Value::String(value))=>Some(value.clone()),_=>anyhow::bail!("missing nullable pending ciphertext"),
    };
    let wrapped=if target.kind==Kind::RateConfig {
        ensure!(target.id==creds.pi_id && row.get("wrappedKey")==Some(&Value::Null)
            && row.get("recordVersion")==Some(&Value::Null),"invalid pending rate identity");
        revision::valid_envelope(ciphertext.as_deref().context("pending rates unavailable")?,16384,None)?;None
    } else {
        ensure!(row["recordVersion"].as_str().is_some_and(route_pull::hex_id),"invalid pending upload source");
        let key=row["wrappedKey"].as_str().context("missing pending wrapped key")?;
        revision::valid_envelope(key,61,Some(61))?;
        if let Some(ct)=&ciphertext {revision::valid_envelope(ct,2048,None)?;}
        Some(key.to_string())
    };
    Ok(Some(Item {kind:target.kind,id:target.id.clone(),revision:"0".into(),updated_at_ms:at,ciphertext,wrapped_key:wrapped}))
}

pub(super) async fn retry(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<()> {
    let binding=revision::binding(creds)?;
    let (entries,context)={
        let _guard=revision::current_pairing(state,&binding).await?;
        let progress=revision::progress(state)?;
        state.store.with_locked_conn(|conn| -> Result<_> {Ok((incoming_retry::entries(conn,&binding)?,Context {
            progress,retry_generation:Some(incoming_retry::generation(conn,&binding)?),
        }))})?
    };
    if entries.is_empty() {return Ok(())}
    let nonroutes:Vec<_>=entries.iter().filter(|entry|entry.target.kind!=Kind::Route).map(|entry|entry.target.clone()).collect();
    let states=match read_states(state,client,creds,&nonroutes).await {
        Ok(states)=>states,
        Err(error)=>{
            let _guard=guard(state,&binding,&context).await?;
            state.store.with_locked_conn(|conn|incoming_retry::advance(conn,&binding,entries.last().unwrap()))?;
            return Err(error)
        }
    };
    let mut items=Vec::new();let mut done=Vec::new();
    for entry in &entries {
        if entry.target.kind==Kind::Route {
            items.push(Item {kind:Kind::Route,id:entry.target.id.clone(),revision:"0".into(),updated_at_ms:0,ciphertext:None,wrapped_key:None});
        } else {
            let row=&states[&format!("{}:{}",entry.target.kind.name(),entry.target.id)];
            match current_item(&entry.target,row,creds) {
                Ok(Some(item))=>items.push(item),Ok(None)=>done.push(entry.target.clone()),Err(_)=>{},
            }
        }
    }
    let (applied,_)=apply(state,client,creds,pi_key,items,&context).await?;done.extend(applied);
    let _guard=guard(state,&binding,&context).await?;
    state.store.with_locked_conn(|conn| -> Result<()> {
        let tx=conn.unchecked_transaction()?;
        for entry in &entries {if done.contains(&entry.target) {incoming_retry::complete(&tx,&binding,entry)?;}}
        incoming_retry::advance(&tx,&binding,entries.last().unwrap())?;
        tx.commit()?;Ok(())
    })?;
    Ok(())
}

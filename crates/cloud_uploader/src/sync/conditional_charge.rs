//! Charging edits merge recorded field operations into a fresh Cloud document.
//! Persist the exact encrypted proposal before sending; an interrupted request
//! is read back on restart, never rebuilt and resent as a new write.
use std::{collections::HashSet, sync::Arc};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sentryusb_cloud_crypto::{aad, credentials::CloudCredentialsV1};
use sentryusb_drives::{db::ChargeMutableSyncSnapshot, schema};
use crate::{client::CloudClient, encrypt::{self, ChargeMutable}, state::CloudStateInner};
use super::{merge_intent, revision};

#[cfg(test)]
pub(super) mod tests;

fn nullable<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where D: Deserializer<'de>, T: Deserialize<'de> { Option::<T>::deserialize(d) }

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Proposal {
    kind: String,
    id: String,
    record_version: String,
    wrapped_key: String,
    #[serde(deserialize_with = "nullable")]
    expected_ciphertext: Option<String>,
    // Always retain an encrypted object, including an empty mutable. Its random
    // nonce distinguishes a confirmed write from another actor's null clear.
    ciphertext: String,
    changed_at_ms: i64,
    field_merge: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    version: u8,
    binding: String,
    session_ts: i64,
    uploaded_at: i64,
    proposal: Proposal,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Remote {
    Ok {
        kind: String,
        id: String,
        #[serde(rename = "recordVersion")]
        record_version: String,
        #[serde(rename = "wrappedKey")]
        wrapped_key: String,
        #[serde(deserialize_with = "nullable")]
        ciphertext: Option<String>,
        #[serde(rename = "updatedAtMs")]
        updated_at_ms: i64,
    },
    NotFound { kind: String, id: String },
}
impl Remote {
    fn identity(&self) -> (&str, &str) {
        match self { Self::Ok {kind,id,..} | Self::NotFound {kind,id} => (kind,id) }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct States { ok: bool, write_protocol: u8, items: Vec<Remote> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Acks { ok: bool, write_protocol: u8, results: Vec<Ack> }
#[derive(Deserialize)]
struct Ack { kind: String, id: String, status: String }

fn hex_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn receipt_key(binding: &str, ts: i64) -> String { format!("cloud_charge_publication_v2:{binding}:{ts}") }

async fn read_states(state: &CloudStateInner, client: &CloudClient, creds: &CloudCredentialsV1,
    binding: &str, ids: &[String]) -> Result<Vec<Remote>> {
    ensure!(!ids.is_empty() && ids.len() <= 200 && ids.iter().all(|id| hex_id(id)), "invalid charging state request");
    { let _guard = revision::current_pairing(state, binding).await?; }
    let response = client.post_json_bearer("/api/pi/sync/state", &json!({
        "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,
        "items":ids.iter().map(|id| json!({"kind":"charge","id":id})).collect::<Vec<_>>()
    })).await.context("read charging state")?;
    ensure!(response.status().is_success(), "Cloud charging state read failed ({})", response.status());
    let parsed: States = serde_json::from_slice(&revision::bounded_body(response).await?).context("invalid charging state response")?;
    let wanted: HashSet<_> = ids.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    ensure!(parsed.ok && parsed.write_protocol == 3 && parsed.items.len() == ids.len(), "incomplete charging state response");
    for item in &parsed.items {
        let (kind,id) = item.identity();
        ensure!(kind == "charge" && wanted.contains(id) && seen.insert(id), "unexpected charging state response");
        if let Remote::Ok {record_version,wrapped_key,ciphertext,updated_at_ms,..} = item {
            ensure!(hex_id(record_version) && (0..=8_640_000_000_000_000).contains(updated_at_ms), "invalid charging source state");
            revision::valid_envelope(wrapped_key, 61, Some(61))?;
            if let Some(ct) = ciphertext { revision::valid_envelope(ct,2048,None)?; }
        }
    }
    { let _guard = revision::current_pairing(state, binding).await?; }
    Ok(parsed.items)
}

fn decrypt(creds: &CloudCredentialsV1, pi_key: &[u8;32], id: &str, wrapped: &str, ct: Option<&str>) -> Result<Value> {
    let key = encrypt::unwrap_content_key(pi_key, wrapped, &aad::charge_key(&creds.user_id,&creds.pi_id,id))?;
    match ct {
        Some(ct) => encrypt::open_json_b64(&key,&aad::charge_mutable(&creds.user_id,&creds.pi_id,id),ct),
        None => Ok(json!({"tags":[],"costOverride":null})),
    }
}

fn fields(value: &Value) -> Result<ChargeMutable> {
    let fields: ChargeMutable = serde_json::from_value(value.clone()).context("invalid charging fields")?;
    if let Some(cost) = &fields.cost_override { ensure!(cost.amount.is_finite() && cost.amount >= 0.0, "invalid charging cost"); }
    Ok(fields)
}

fn confirm(state: &CloudStateInner, ts: i64, through: i64, id: &str, wrapped: &str,
    uploaded: i64, value: &Value, receipt: Option<(&str,&str)>) -> Result<()> {
    let fields = fields(value)?;
    ensure!(state.store.confirm_charge_mutable_push_with_receipt(ts,through,id,wrapped,uploaded,
        &fields.tags,fields.cost_override.map(|c| (c.amount,c.currency)),receipt)?,
        "charging source or pending edit changed before confirmation");
    Ok(())
}

fn matches_source(pending: &Pending, remote: &Remote) -> bool {
    matches!(remote, Remote::Ok {id,record_version,wrapped_key,..}
        if id == &pending.proposal.id && record_version == &pending.proposal.record_version && wrapped_key == &pending.proposal.wrapped_key)
}

fn retire_attempt(state: &CloudStateInner, key: &str, expected: &str) -> Result<()> {
    state.store.with_locked_conn(|conn| {
        let tx=conn.unchecked_transaction()?;
        ensure!(schema::meta_get(&tx,key)?.as_deref() == Some(expected), "charging publication changed");
        schema::meta_del(&tx,key)?; tx.commit()?; Ok(())
    })
}

async fn post(state: &CloudStateInner, client: &CloudClient, creds: &CloudCredentialsV1,
    binding: &str, proposals: &[&Proposal]) -> Result<Acks> {
    { let _guard=revision::current_pairing(state,binding).await?; }
    let response=client.post_json_bearer("/api/pi/sync/mutables/v3",&json!({
        "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,"items":proposals
    })).await.context("conditional charging publication")?;
    ensure!(response.status().is_success(),"charging publication unconfirmed");
    let acks: Acks=serde_json::from_slice(&revision::bounded_body(response).await?)?;
    let wanted: HashSet<_>=proposals.iter().map(|p|p.id.as_str()).collect();
    let mut seen=HashSet::new();
    ensure!(acks.ok && acks.write_protocol == 3 && acks.results.len() == proposals.len()
        && acks.results.iter().all(|ack| ack.kind == "charge" && wanted.contains(ack.id.as_str())
            && seen.insert(ack.id.as_str()) && matches!(ack.status.as_str(),"applied"|"conflict"|"source_changed"|"not_found")),
        "invalid charging acknowledgement");
    Ok(acks)
}

async fn recover(state: &CloudStateInner, client: &CloudClient, creds: &CloudCredentialsV1,
    pi_key: &[u8;32], binding: &str, key: &str, raw: &str) -> Result<()> {
    let pending: Pending = serde_json::from_str(raw).context("unreadable pending charging publication")?;
    ensure!(matches!(pending.version,2|3) && pending.binding == binding && pending.proposal.kind == "charge"
        && key == receipt_key(binding,pending.session_ts) && pending.proposal.field_merge,
        "pending charging publication identity changed");
    revision::valid_envelope(&pending.proposal.ciphertext,2048,None)?;
    let mut states=read_states(state,client,creds,binding,&[pending.proposal.id.clone()]).await?;
    let exact=matches_source(&pending,&states[0]) && matches!(&states[0],Remote::Ok {ciphertext:Some(ct),..} if ct == &pending.proposal.ciphertext);
    if !exact {
        // Version 2 attempts predate server operation receipts and cannot be
        // resent safely. Version 3 repeats the original operation unchanged.
        ensure!(pending.version == 3 && pending.proposal.operation_id.as_deref().is_some_and(hex_id),
            "older charging save remains unconfirmed; reconciliation required");
        let acks=post(state,client,creds,binding,&[&pending.proposal]).await?;
        if acks.results[0].status != "applied" {
            let _guard=revision::current_pairing(state,binding).await?;
            retire_attempt(state,key,raw)?;
            anyhow::bail!("charging operation was rejected; a fresh merge is required");
        }
        // A receipt may refer to a save followed by another Cloud edit. Adopt
        // the current document, not the older confirmed proposal's plaintext.
        states=read_states(state,client,creds,binding,&[pending.proposal.id.clone()]).await?;
    }
    ensure!(matches_source(&pending,&states[0]),"charging source changed after confirmed publication");
    let Remote::Ok {ciphertext,..}=&states[0] else { unreachable!() };
    let value=decrypt(creds,pi_key,&pending.proposal.id,&pending.proposal.wrapped_key,ciphertext.as_deref())?;
    let _guard=revision::current_pairing(state,binding).await?;
    confirm(state,pending.session_ts,pending.proposal.changed_at_ms,&pending.proposal.id,&pending.proposal.wrapped_key,
        pending.uploaded_at,&value,Some((key,raw)))
}

struct Ready { pending: Pending, key: String, raw: String }

pub(super) async fn push(state: &Arc<CloudStateInner>, client: &CloudClient,
    creds: &CloudCredentialsV1, pi_key: &[u8;32]) -> Result<()> {
    let binding=revision::binding(creds)?;
    { let _guard=revision::current_pairing(state,&binding).await?; }
    let mut snapshots: Vec<(i64,i64,ChargeMutableSyncSnapshot)>=Vec::new();
    let mut failed=false;
    for (kind,key,through) in state.store.dirty_mutables()? {
        if kind != "charge" { continue }
        let ts=key.parse::<i64>().context("invalid queued charging session")?;
        let receipt=receipt_key(&binding,ts);
        if let Some(raw)=state.store.with_locked_conn(|conn| schema::meta_get(conn,&receipt))? {
            // Recover the original operation, including after a process restart.
            if recover(state,client,creds,pi_key,&binding,&receipt,&raw).await.is_err() { failed=true; }
            continue;
        }
        let Some(snapshot)=state.store.charge_mutable_for_sync(ts,through)? else { continue };
        if snapshot.upload.as_ref().is_some_and(|(_,_,at)| *at >= 0) { snapshots.push((ts,through,snapshot)); }
    }
    // Targeted state reads and writes share the server's maximum batch size.
    let ids: HashSet<_>=snapshots.iter().map(|(_,_,s)| &s.upload.as_ref().unwrap().0).collect();
    ensure!(ids.len() == snapshots.len(), "multiple local sessions target the same Cloud charging session");
    for chunk in snapshots.chunks(200) {
        let ids: Vec<_>=chunk.iter().map(|(_,_,s)| s.upload.as_ref().unwrap().0.clone()).collect();
        let remotes=read_states(state,client,creds,&binding,&ids).await?;
        let mut ready=Vec::new();
        let preparation_guard=revision::current_pairing(state,&binding).await?;
        for (ts,through,snapshot) in chunk {
            let (id,wrapped,uploaded)=snapshot.upload.as_ref().unwrap();
            let remote=remotes.iter().find(|r| r.identity().1 == id).context("missing charging state")?;
            let prepared: Result<Option<Ready>>=(|| {
                let Remote::Ok {record_version,wrapped_key,ciphertext,..}=remote else { anyhow::bail!("Cloud charging source is missing") };
                ensure!(wrapped.is_empty() || wrapped_key == wrapped,"Cloud charging source key changed");
                let cloud=decrypt(creds,pi_key,id,wrapped_key,ciphertext.as_deref())?;
                if wrapped.is_empty() {
                    ensure!(state.store.backfill_charge_upload_key(*ts,id,*uploaded,wrapped_key)?,"charge upload changed during key backfill");
                }
                let wrapped=wrapped_key;
                let cloud_fields=fields(&cloud)?;
                let merged=if snapshot.intent.legacy {
                    // Lost field history cannot justify replacing remote values.
                    let local_tags: std::collections::BTreeSet<_>=snapshot.tags.iter().collect();
                    let cloud_tags: std::collections::BTreeSet<_>=cloud_fields.tags.iter().collect();
                    ensure!(local_tags == cloud_tags && snapshot.cost == cloud_fields.cost_override.map(|c| (c.amount,c.currency)),
                        "older queued charging edit needs reconciliation");
                    cloud.clone()
                } else { merge_intent::merge_charge(&snapshot.intent,&cloud)? };
                fields(&merged)?;
                if merged == cloud {
                    confirm(state,*ts,*through,id,wrapped,*uploaded,&merged,None)?;
                    return Ok(None)
                }
                let content_key=encrypt::unwrap_content_key(pi_key,wrapped,&aad::charge_key(&creds.user_id,&creds.pi_id,id))?;
                let desired=encrypt::seal_json_b64(&content_key,&aad::charge_mutable(&creds.user_id,&creds.pi_id,id),&merged)?;
                revision::valid_envelope(&desired,2048,None)?;
                let mut pending=Pending {version:3,binding:binding.clone(),session_ts:*ts,uploaded_at:*uploaded,
                    proposal:Proposal {kind:"charge".into(),id:id.clone(),record_version:record_version.clone(),wrapped_key:wrapped.clone(),
                        expected_ciphertext:ciphertext.clone(),ciphertext:desired,changed_at_ms:*through,field_merge:true,operation_id:None}};
                let identity=serde_json::to_vec(&(&binding,&pending.proposal))?;
                pending.proposal.operation_id=Some(ring::digest::digest(&ring::digest::SHA256,&identity)
                    .as_ref().iter().map(|b|format!("{b:02x}")).collect());
                let key=receipt_key(&binding,*ts); let raw=serde_json::to_string(&pending)?;
                // Atomically reserve the attempt and recheck its local source.
                state.store.with_durable_conn(|conn| {
                    let tx=conn.unchecked_transaction()?;
                    ensure!(schema::meta_get(&tx,&key)?.is_none(),"charging publication already pending");
                    let valid: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM charge_uploads u JOIN mutable_dirty d ON d.kind='charge' AND d.key=CAST(u.session_ts AS TEXT) WHERE u.session_ts=?1 AND u.cloud_charge_id=?2 AND u.wrapped_charge_key=?3 AND u.uploaded_at=?4 AND d.changed_at>=?5)",
                        rusqlite::params![ts,id,wrapped,uploaded,through],|r| r.get(0))?;
                    ensure!(valid,"charging source changed before publication");
                    schema::meta_set(&tx,&key,&raw)?; tx.commit()?; Ok(())
                })?;
                Ok(Some(Ready {pending,key,raw}))
            })();
            match prepared { Ok(Some(item))=>ready.push(item), Ok(None)=>{}, Err(_)=>failed=true }
        }
        drop(preparation_guard);
        if ready.is_empty() { continue }
        { let _guard=revision::current_pairing(state,&binding).await?; }
        let proposals: Vec<_>=ready.iter().map(|r| &r.pending.proposal).collect();
        let result=post(state,client,creds,&binding,&proposals).await;
        match result {
            Ok(acks)=>{
                let _guard=revision::current_pairing(state,&binding).await?;
                for item in ready {
                    let ack=acks.results.iter().find(|a| a.id == item.pending.proposal.id).unwrap();
                    if ack.status == "applied" {
                        let p=&item.pending.proposal;
                        let value=decrypt(creds,pi_key,&p.id,&p.wrapped_key,Some(&p.ciphertext))?;
                        if confirm(state,item.pending.session_ts,p.changed_at_ms,&p.id,&p.wrapped_key,item.pending.uploaded_at,
                            &value,Some((&item.key,&item.raw))).is_err() { failed=true; }
                    } else {
                        // A definite rejection permits a new read/merge next sweep.
                        retire_attempt(state,&item.key,&item.raw)?; failed=true;
                    }
                }
            }
            Err(_)=>{
                for item in ready {
                    if recover(state,client,creds,pi_key,&binding,&item.key,&item.raw).await.is_err() { failed=true; }
                }
            }
        }
    }
    ensure!(!failed,"some charging edits need reconciliation and remain queued");
    Ok(())
}

//! Durable, per-member publication of one drive's scoped tag edits.
//! Confirmed members are not rewritten when another member needs a fresh merge.
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sentryusb_cloud_crypto::{aad, credentials::CloudCredentialsV1};
use sentryusb_drives::{mutable_intent::{Edit, Intent}, schema};
use crate::{client::CloudClient, encrypt, state::CloudStateInner};
use super::{revision, scoped_route_tags, route_pull::{self, Remote, Snapshot}};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
struct Proposal {
    kind:String,id:String,record_version:String,wrapped_key:String,
    expected_ciphertext:Option<String>,ciphertext:String,changed_at_ms:i64,
    field_merge:bool,operation_id:String,expected_tags_format_version:u8,
    tags_format_version:u8,expected_summary_ciphertext:String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="camelCase")]
enum Outcome { Pending, Applied, Rejected }
#[derive(Clone, Serialize, Deserialize)]
struct Member {
    file:String,record_version:String,wrapped_key:String,
    proposal:Option<Proposal>,outcome:Outcome,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    version:u8,binding:String,drive:String,through:i64,intent:Intent,
    local:Snapshot,members:Vec<Member>,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Acks {ok:bool,write_protocol:u8,results:Vec<Ack>}
#[derive(Deserialize)]
struct Ack {kind:String,id:String,status:String}

fn receipt_key(binding:&str,drive:&str)->String {format!("cloud_drive_publication_v2:{binding}:{drive}")}
fn ids(local:&Snapshot)->Vec<String> {
    let mut ids:Vec<_>=local.sources.values().map(|source|source.id.clone()).collect();ids.sort();ids
}
fn same_local(original:&Snapshot,current:&Snapshot)->bool {
    original.drives==current.drives && original.sources.len()==current.sources.len()
        && original.sources.iter().all(|(file,source)|current.sources.get(file).is_some_and(|fresh|
            source.id==fresh.id && source.uploaded_at==fresh.uploaded_at && source.runs==fresh.runs
            && fresh.key.as_ref().is_none_or(|key|source.key.as_ref()==Some(key))))
}
fn check_source(member:&Member,remote:&Remote)->Result<()> {
    ensure!(matches!(remote,Remote::Ok {record_version,wrapped_key,..}
        if record_version==&member.record_version && wrapped_key==&member.wrapped_key),"drive source replaced while tags were pending");
    Ok(())
}

fn prepare_member(local:&Snapshot,file:&str,remote:&Remote,intent:&Intent,through:i64,
    creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<Member> {
    ensure!(!intent.legacy,"older drive tag edit needs reconciliation");
    let source=local.sources.get(file).context("missing original drive source")?;
    let Remote::Ok {id,record_version,wrapped_key,ciphertext,summary,format,..}=remote
        else {anyhow::bail!("drive member is unavailable to this pairing")};
    ensure!(id==&source.id && source.key.as_ref().is_none_or(|key|key==wrapped_key),"drive source identity changed");
    let key=encrypt::unwrap_content_key(pi_key,wrapped_key,&aad::route_key(&creds.user_id,&creds.pi_id,id))?;
    let before:Value=match ciphertext {
        Some(ct)=>encrypt::open_json_b64(&key,&aad::route_tags(&creds.user_id,&creds.pi_id,id),ct)?,
        None=>json!([]),
    };
    ensure!((*format==1 && before.is_array()) || (*format==2 && before.is_object()),"drive tag format mismatch");
    let windows:Vec<_>=local.drives.iter().flat_map(|(_,windows)|windows.iter()).filter(|window|window.file==file).collect();
    let total=windows.first().context("missing selected source frames")?.total_frames;
    ensure!(windows.iter().all(|window|window.total_frames==total),"inconsistent source frame counts");
    let ranges:Vec<_>=windows.iter().map(|window|(window.start_frame,window.end_frame)).collect();
    let original=scoped_route_tags::apply_delta(&before,total,&ranges,&[],&[])?;
    let mut desired=original.clone();
    for (_,edit) in &intent.edits {
        let Edit::Tags {added,removed}=edit else {anyhow::bail!("unsupported drive tag intent")};
        desired=scoped_route_tags::apply_delta(&desired,total,&ranges,added,removed)?;
    }
    let mut member=Member {file:file.into(),record_version:record_version.clone(),wrapped_key:wrapped_key.clone(),
        proposal:None,outcome:Outcome::Applied};
    if desired==original {return Ok(member)}
    let summary=summary.as_ref().context("drive source summary unavailable for scoped edit")?;
    let decoded:Value=encrypt::open_json_b64(&key,&aad::route_summary(&creds.user_id,&creds.pi_id,id),summary)?;
    ensure!(decoded["file"].as_str()==Some(file) && decoded["gr"]==serde_json::to_value(&source.runs)?,"drive source frames changed");
    let frames=source.runs.chunks_exact(2).try_fold(0u32,|total,run| {
        ensure!(run[0]<=255 && run[1]>0,"invalid original frame runs");
        total.checked_add(run[1]).context("frame count overflow")
    })?.max(1);
    ensure!(frames==total,"drive source frame count changed");
    let ciphertext=encrypt::seal_json_b64(&key,&aad::route_tags(&creds.user_id,&creds.pi_id,id),&desired)?;
    revision::valid_envelope(&ciphertext,2048,None)?;
    let mut proposal=Proposal {kind:"route".into(),id:id.clone(),record_version:record_version.clone(),wrapped_key:wrapped_key.clone(),
        expected_ciphertext:ciphertext_option(remote),ciphertext,changed_at_ms:through,field_merge:true,operation_id:String::new(),
        expected_tags_format_version:*format,tags_format_version:2,expected_summary_ciphertext:summary.clone()};
    let identity=serde_json::to_vec(&(&creds.user_id,&creds.pi_id,&proposal))?;
    proposal.operation_id=ring::digest::digest(&ring::digest::SHA256,&identity).as_ref().iter().map(|byte|format!("{byte:02x}")).collect();
    member.proposal=Some(proposal);member.outcome=Outcome::Pending;
    Ok(member)
}
fn ciphertext_option(remote:&Remote)->Option<String> {
    match remote {Remote::Ok {ciphertext,..}=>ciphertext.clone(),Remote::NotFound {..}=>None}
}

fn save(state:&CloudStateInner,key:&str,expected:Option<&str>,pending:&Pending,source_revision:Option<i64>)->Result<String> {
    let raw=serde_json::to_string(pending)?;
    state.store.with_durable_conn(|conn| {
        let tx=conn.unchecked_transaction()?;
        ensure!(schema::meta_get(&tx,key)?.as_deref()==expected,"another drive publication changed the receipt");
        let through:Option<i64>=tx.query_row("SELECT changed_at FROM mutable_dirty WHERE kind='drive' AND key=?1",[&pending.drive],|row|row.get(0))
            .optional()?;
        ensure!(through.is_some_and(|through|through>=pending.through),"drive intent changed before receipt publication");
        if let Some(expected)=source_revision {
            let current:i64=tx.query_row("SELECT revision FROM mutable_route_source_clock WHERE id=1",[],|row|row.get(0))?;
            ensure!(current==expected,"drive source changed before receipt publication");
        }
        schema::meta_set(&tx,key,&raw)?;tx.commit()?;Ok(())
    })?;
    Ok(raw)
}
use rusqlite::OptionalExtension;

async fn post(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,binding:&str,proposals:&[&Proposal])->Result<Acks> {
    {let _guard=revision::current_pairing(state,binding).await?;}
    let response=client.post_json_bearer("/api/pi/sync/mutables/v3",&json!({
        "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,"items":proposals
    })).await.context("publish scoped drive tags")?;
    ensure!(response.status().is_success(),"scoped drive publication unconfirmed");
    let acks:Acks=serde_json::from_slice(&revision::bounded_body(response).await?)?;
    let wanted:HashSet<_>=proposals.iter().map(|proposal|proposal.id.as_str()).collect();let mut seen=HashSet::new();
    ensure!(acks.ok && acks.write_protocol==3 && acks.results.len()==proposals.len() && acks.results.iter().all(|ack|
        ack.kind=="route" && wanted.contains(ack.id.as_str()) && seen.insert(ack.id.as_str())
        && matches!(ack.status.as_str(),"applied"|"conflict"|"source_changed"|"not_found")),"invalid drive publication acknowledgement");
    Ok(acks)
}

fn validate(pending:&Pending,binding:&str,drive:&str)->Result<()> {
    ensure!(pending.version==1 && pending.binding==binding && pending.drive==drive && pending.through>0
        && pending.local.drives.len()==1 && pending.local.drives[0].0==drive
        && pending.members.len()==pending.local.sources.len() && !pending.members.is_empty(),"invalid pending drive publication");
    let mut files=HashSet::new();let mut operations=HashSet::new();
    for member in &pending.members {
        let source=pending.local.sources.get(&member.file).context("pending drive member changed")?;
        ensure!(files.insert(&member.file) && source.key.as_ref()==Some(&member.wrapped_key)
            && route_pull::hex_id(&source.id) && route_pull::hex_id(&member.record_version),"invalid pending drive source");
        if let Some(proposal)=&member.proposal {
            ensure!(proposal.kind=="route" && proposal.id==source.id && proposal.record_version==member.record_version
                && proposal.wrapped_key==member.wrapped_key && proposal.changed_at_ms==pending.through && proposal.field_merge
                && route_pull::hex_id(&proposal.operation_id) && operations.insert(&proposal.operation_id)
                && matches!(proposal.expected_tags_format_version,1|2) && proposal.tags_format_version==2,"invalid pending drive operation");
            revision::valid_envelope(&proposal.wrapped_key,61,Some(61))?;
            revision::valid_envelope(&proposal.ciphertext,2048,None)?;
            if let Some(expected)=&proposal.expected_ciphertext {revision::valid_envelope(expected,2048,None)?;}
            revision::valid_envelope(&proposal.expected_summary_ciphertext,4096,None)?;
        } else {ensure!(member.outcome==Outcome::Applied,"pending drive member has no proposal");}
    }
    Ok(())
}

async fn finish(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
    key:&str,raw:&str,pending:&Pending,attempt_revision:i64)->Result<()> {
    ensure!(state.store.mutable_route_source_revision()?==attempt_revision,"drive sources changed during publication");
    let fresh=route_pull::snapshot_for_drive(state,&pending.drive)?.context("pending drive no longer exists")?;
    ensure!(fresh.revision==attempt_revision && same_local(&pending.local,&fresh),"pending drive windows or source changed");
    let current=route_pull::snapshot_for_projection(state,&fresh)?;
    let mut wanted=ids(&fresh);wanted.extend(ids(&current));wanted.sort();wanted.dedup();
    let states=route_pull::read_states(state,client,creds,&pending.binding,&wanted).await?;
    for member in &pending.members {check_source(member,states.get(&fresh.sources[&member.file].id).context("missing confirmed member")?)?;}
    let projection=route_pull::project(fresh,states.clone(),creds,pi_key)?;
    let mut current_projection=route_pull::project(current,states,creds,pi_key)?;
    let current_files:HashSet<_>=current_projection.sources.iter().map(|source|source.file.clone()).collect();
    current_projection.sources.extend(projection.sources.into_iter().filter(|source|!current_files.contains(&source.file)));
    let tags=&projection.drives.first().context("confirmed drive missing")?.1;
    let _guard=revision::current_pairing(state,&pending.binding).await?;
    ensure!(state.store.confirm_drive_mutable_push_with_projection(&pending.drive,pending.through,projection.revision,
        tags,&current_projection.sources,Some((key,raw)),&current_projection.drives)?,"drive source or receipt changed before confirmation");
    Ok(())
}

async fn advance(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32],
    key:&str,mut raw:String,mut pending:Pending)->Result<()> {
    validate(&pending,&revision::binding(creds)?,&pending.drive)?;
    let fresh=route_pull::snapshot_for_drive(state,&pending.drive)?.context("pending drive no longer exists")?;
    ensure!(same_local(&pending.local,&fresh),"pending drive source windows changed");
    if pending.members.iter().any(|member|member.outcome==Outcome::Rejected) {
        let states=route_pull::read_states(state,client,creds,&pending.binding,&ids(&fresh)).await?;
        // Authenticate every member before preparing a retry. Confirmed members
        // stay confirmed, even if someone subsequently changed their Cloud tags.
        route_pull::project(fresh.clone(),states.clone(),creds,pi_key)?;
        for member in &mut pending.members {
            if member.outcome!=Outcome::Rejected {continue}
            let remote=states.get(&fresh.sources[&member.file].id).context("missing retry member")?;
            check_source(member,remote)?;
            *member=prepare_member(&fresh,&member.file,remote,&pending.intent,pending.through,creds,pi_key)?;
        }
        let _guard=revision::current_pairing(state,&pending.binding).await?;
        raw=save(state,key,Some(&raw),&pending,Some(fresh.revision))?;
    }
    let waiting:Vec<_>=pending.members.iter().enumerate().filter(|(_,member)|member.outcome==Outcome::Pending).map(|(index,_)|index).collect();
    for chunk in waiting.chunks(100) {
        {
            let _guard=revision::current_pairing(state,&pending.binding).await?;
            state.store.with_locked_conn(|conn| -> Result<()> {
                ensure!(schema::meta_get(conn,key)?.as_deref()==Some(raw.as_str()),"drive receipt changed before transmission");
                let current:i64=conn.query_row("SELECT revision FROM mutable_route_source_clock WHERE id=1",[],|row|row.get(0))?;
                ensure!(current==fresh.revision,"drive source changed before transmission");
                Ok(())
            })?;
        }
        let proposals:Vec<_>=chunk.iter().map(|&index|pending.members[index].proposal.as_ref().unwrap()).collect();
        let acks=post(state,client,creds,&pending.binding,&proposals).await?;
        for &index in chunk {
            let member=&mut pending.members[index];let id=&member.proposal.as_ref().unwrap().id;
            let ack=acks.results.iter().find(|ack|&ack.id==id).unwrap();
            member.outcome=if ack.status=="applied" {Outcome::Applied}else{Outcome::Rejected};
        }
        let _guard=revision::current_pairing(state,&pending.binding).await?;
        raw=save(state,key,Some(&raw),&pending,None)?;
    }
    ensure!(pending.members.iter().all(|member|member.outcome==Outcome::Applied),"some drive members need a fresh merge");
    finish(state,client,creds,pi_key,key,&raw,&pending,fresh.revision).await
}

pub(super) async fn push(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<()> {
    let binding=revision::binding(creds)?;let mut failed=false;
    for (kind,drive,through) in state.store.dirty_mutables()? {
        if kind!="drive" {continue}
        let result:Result<()>=async {
            {let _guard=revision::current_pairing(state,&binding).await?;}
            let key=receipt_key(&binding,&drive);
            if let Some(raw)=state.store.with_locked_conn(|conn|schema::meta_get(conn,&key))? {
                let pending:Pending=serde_json::from_str(&raw).context("unreadable pending drive publication")?;
                validate(&pending,&binding,&drive)?;
                return advance(state,client,creds,pi_key,&key,raw,pending).await
            }
            let local=route_pull::snapshot_for_drive(state,&drive)?.context("queued drive no longer exists")?;
            let Some(edit)=state.store.drive_mutable_for_sync(&drive,through)? else {return Ok(())};
            let states=route_pull::read_states(state,client,creds,&binding,&ids(&local)).await?;
            let projection=route_pull::project(local.clone(),states.clone(),creds,pi_key)?;
            if edit.intent.legacy {
                ensure!(edit.tags.iter().collect::<BTreeSet<_>>()==projection.drives[0].1.iter().collect(),"older drive tag edit needs reconciliation");
                let _guard=revision::current_pairing(state,&binding).await?;
                ensure!(state.store.confirm_drive_mutable_push_with_receipt(&drive,through,projection.revision,
                    &projection.drives[0].1,&projection.sources,None)?,"legacy drive source changed");
                return Ok(())
            }
            let mut pending=Pending {version:1,binding:binding.clone(),drive:drive.clone(),through,intent:edit.intent,local,members:Vec::new()};
            let mut files:Vec<_>=pending.local.sources.keys().cloned().collect();files.sort();
            for file in files {
                let member=prepare_member(&pending.local,&file,&states[&pending.local.sources[&file].id],&pending.intent,through,creds,pi_key)?;
                pending.local.sources.get_mut(&file).unwrap().key=Some(member.wrapped_key.clone());
                pending.members.push(member);
            }
            let raw={let _guard=revision::current_pairing(state,&binding).await?;save(state,&key,None,&pending,Some(pending.local.revision))?};
            advance(state,client,creds,pi_key,&key,raw,pending).await
        }.await;
        if result.is_err() {failed=true;}
    }
    ensure!(!failed,"some drive tag edits remain queued for reconciliation");
    Ok(())
}

#[cfg(test)]
mod tests;

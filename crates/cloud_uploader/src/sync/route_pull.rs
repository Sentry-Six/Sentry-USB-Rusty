//! Project Cloud tags over every original member window of an affected drive.
//! A clip-wide response must never replace a multi-clip drive's entire tag set.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sentryusb_cloud_crypto::{aad, credentials::CloudCredentialsV1};
use sentryusb_drives::{db::VerifiedRouteSyncKey, grouper::{self, DriveClipWindow}};

use crate::{client::CloudClient, encrypt, state::CloudStateInner};
use super::{RouteChange, revision, scoped_route_tags};

fn nullable<'de,D,T>(deserializer: D)->std::result::Result<Option<T>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> { Option::<T>::deserialize(deserializer) }

#[derive(Clone, Deserialize)]
#[serde(tag="status",rename_all="snake_case")]
pub(super) enum Remote {
    Ok {
        kind: String, id: String,
        #[serde(rename="recordVersion")] record_version: String,
        #[serde(rename="wrappedKey")] wrapped_key: String,
        #[serde(deserialize_with="nullable")] ciphertext: Option<String>,
        #[serde(rename="summaryCiphertext",deserialize_with="nullable")] summary: Option<String>,
        #[serde(rename="tagsFormatVersion")] format: u8,
        #[serde(rename="updatedAtMs")] updated_at: i64,
    },
    NotFound {kind:String,id:String},
}
impl Remote {
    fn identity(&self)->(&str,&str) {
        match self {Self::Ok {kind,id,..}|Self::NotFound {kind,id} => (kind,id)}
    }
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct States {ok:bool,write_protocol:u8,items:Vec<Remote>}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Source {
    pub(super) id:String,
    pub(super) key:Option<String>,
    pub(super) uploaded_at:i64,
    pub(super) runs:Vec<u32>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub(super) revision:i64,
    pub(super) drives:Vec<(String,Vec<DriveClipWindow>)>,
    pub(super) sources:HashMap<String,Source>,
}
pub(super) struct Projection {
    pub(super) revision:i64,
    pub(super) sources:Vec<VerifiedRouteSyncKey>,
    pub(super) drives:Vec<(String,Vec<String>)>,
}
impl Projection {
    pub(super) fn apply(self,state:&CloudStateInner)->Result<()> {
        state.store.apply_projected_drive_tags(self.revision,&self.sources,&self.drives)
    }
}
pub(super) fn hex_id(id:&str)->bool {
    id.len()==64 && id.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn snapshot(state:&CloudStateInner,ids:&[String])->Result<Option<Snapshot>> {
    let revision=state.store.mutable_route_source_revision()?;
    let mut changed_files=HashSet::new();
    for id in ids {
        if let Some((file,_))=state.store.route_sync_info_by_cloud_id(id)? {
            changed_files.insert(file);
        }
    }
    if changed_files.is_empty() {return Ok(None)}
    snapshot_matching(state,revision,|_,windows|windows.iter().any(|window|changed_files.contains(&window.file)))
}

pub(super) fn snapshot_for_drive(state:&CloudStateInner,drive:&str)->Result<Option<Snapshot>> {
    let revision=state.store.mutable_route_source_revision()?;
    if let Some(recorded)=state.store.drive_edit_scope(drive)? {
        let old=recorded.context("queued drive had no resolvable frame scope")?;
        // A later clip or grouping correction cannot broaden the original edit.
        let fresh=snapshot_windows(state,revision,vec![(drive.into(),old.windows.clone())])?;
        let current=sentryusb_drives::drive_edit_scope::Scope {
            version:1,windows:fresh.drives[0].1.clone(),
            sources:fresh.sources.iter().map(|(file,source)|(file.clone(),sentryusb_drives::drive_edit_scope::Source {
                runs:source.runs.clone(),id:Some(source.id.clone()),key:source.key.clone(),uploaded_at:Some(source.uploaded_at),
            })).collect(),
        };
        ensure!(old.matches(&current),"queued drive frames or original source changed");
        return Ok(Some(fresh))
    }
    snapshot_matching(state,revision,|key,_|key==drive)
}

// Current display groups are read independently from an edit's frozen scope.
// This also refreshes other drives sharing an edited clip without modifying
// their independent frames in Cloud.
pub(super) fn snapshot_for_projection(state:&CloudStateInner,original:&Snapshot)->Result<Snapshot> {
    snapshot_matching(state,original.revision,|_,windows|windows.iter().any(|window|
        original.sources.contains_key(&window.file)))?.context("edited sources have no current drive grouping")
}

fn snapshot_matching<F>(state:&CloudStateInner,revision:i64,select:F)->Result<Option<Snapshot>>
where F:Fn(&str,&[DriveClipWindow])->bool {
    let drives=state.store.with_route_summaries(|summaries|
        grouper::drive_key_clip_windows(summaries).into_iter()
            .filter(|(key,windows)|select(key,windows)).collect::<Vec<_>>())?;
    if drives.is_empty() {return Ok(None)}
    Ok(Some(snapshot_windows(state,revision,drives)?))
}

fn snapshot_windows(state:&CloudStateInner,revision:i64,drives:Vec<(String,Vec<DriveClipWindow>)>)->Result<Snapshot> {
    ensure!(!drives.is_empty() && drives.iter().all(|(_,windows)|!windows.is_empty()),"empty selected drive scope");
    let files:HashSet<_>=drives.iter().flat_map(|(_,windows)|windows.iter().map(|window|window.file.as_str())).collect();
    let sources=state.store.with_read_conn(|conn| -> Result<HashMap<String,Source>> {
        let mut query=conn.prepare_cached("SELECT cloud_route_id,cloud_wrapped_route_key,cloud_uploaded_at,gear_runs_blob FROM routes WHERE file=?1")?;
        let mut ids=HashSet::new();let mut sources=HashMap::new();
        for file in files {
            let (id,key,uploaded,blob):(Option<String>,Option<String>,Option<i64>,Option<Vec<u8>>)=query.query_row([file],|row|
                Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
            let id=id.context("drive member has not uploaded yet")?;
            ensure!(uploaded.is_some_and(|at|at>0) && hex_id(&id) && ids.insert(id.clone()),"drive member source is not ready for tag sync");
            let runs=sentryusb_drives::blob::decode_gear_runs(blob.as_deref())?.unwrap_or_default().iter()
                .flat_map(|run|[u32::from(run.gear),run.frames]).collect::<Vec<_>>();
            let total=runs.chunks_exact(2).try_fold(0u32,|sum,run| {
                ensure!(run[0]<=255 && run[1]>0,"invalid selected source frame runs");
                sum.checked_add(run[1]).context("selected source frame count overflow")
            })?.max(1);
            ensure!(drives.iter().flat_map(|(_,windows)|windows).filter(|window|window.file==file).all(|window|
                window.total_frames==total && window.start_frame<window.end_frame && window.end_frame<=total),
                "selected drive frame bounds changed");
            sources.insert(file.to_string(),Source {id,key,uploaded_at:uploaded.unwrap(),runs});
        }
        Ok(sources)
    })?;
    ensure!(state.store.mutable_route_source_revision()?==revision,"drive membership changed during tag snapshot");
    Ok(Snapshot {revision,drives,sources})
}

#[derive(Debug)]
pub(super) struct ReadUnavailable(pub(super) u16);
impl std::fmt::Display for ReadUnavailable {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"Cloud drive tag read failed ({})",self.0)}
}
impl std::error::Error for ReadUnavailable {}

pub(super) async fn read_states(state:&CloudStateInner,client:&CloudClient,creds:&CloudCredentialsV1,
    binding:&str,ids:&[String])->Result<HashMap<String,Remote>> {
    let mut states=HashMap::new();
    for chunk in ids.chunks(100) {
        {let _guard=revision::current_pairing(state,binding).await?;}
        let response=client.post_json_bearer("/api/pi/sync/state",&json!({
            "piId":creds.pi_id,"dekRotationGeneration":creds.dek_rotation_generation,
            "items":chunk.iter().map(|id|json!({"kind":"route","id":id})).collect::<Vec<_>>()
        })).await.context("read complete drive tag state")?;
        if !response.status().is_success() {return Err(ReadUnavailable(response.status().as_u16()).into())}
        let parsed:States=serde_json::from_slice(&revision::bounded_body(response).await?).context("decode drive tag state")?;
        ensure!(parsed.ok && parsed.write_protocol==3 && parsed.items.len()==chunk.len(),"incomplete drive tag state");
        let wanted:HashSet<_>=chunk.iter().map(String::as_str).collect();
        for item in parsed.items {
            let (kind,id)=item.identity();
            ensure!(kind=="route" && wanted.contains(id) && !states.contains_key(id),"unexpected drive tag state");
            if let Remote::Ok {record_version,wrapped_key,ciphertext,summary,format,updated_at,..}=&item {
                ensure!(hex_id(record_version) && (0..=8_640_000_000_000_000).contains(updated_at)
                    && (*format==1 || *format==2) && (*format==1 || ciphertext.is_some()),"invalid drive tag source state");
                revision::valid_envelope(wrapped_key,61,Some(61))?;
                if let Some(ct)=ciphertext {revision::valid_envelope(ct,2048,None)?;}
                if let Some(ct)=summary {revision::valid_envelope(ct,4096,None)?;}
            }
            states.insert(id.to_string(),item);
        }
        {let _guard=revision::current_pairing(state,binding).await?;}
    }
    Ok(states)
}

pub(super) fn project(snapshot:Snapshot,states:HashMap<String,Remote>,creds:&CloudCredentialsV1,pi_key:&[u8;32])->Result<Projection> {
    let mut documents=HashMap::new();let mut verified=Vec::new();
    for (file,source) in &snapshot.sources {
        let Remote::Ok {wrapped_key,ciphertext,summary,format,..}=states.get(&source.id).context("missing route tag response")?
            else {anyhow::bail!("drive member is unavailable to this pairing")};
        ensure!(source.key.as_ref().is_none_or(|key|key==wrapped_key),"drive member key changed");
        // Authenticate even a clear operation before publishing key caches or tags.
        let key=encrypt::unwrap_content_key(pi_key,wrapped_key,&aad::route_key(&creds.user_id,&creds.pi_id,&source.id))?;
        let document:Value=match ciphertext {
            Some(ct)=>encrypt::open_json_b64(&key,&aad::route_tags(&creds.user_id,&creds.pi_id,&source.id),ct)?,
            None=>json!([]),
        };
        if *format==1 {
            ensure!(document.as_array().is_some_and(|tags|tags.iter().all(Value::is_string)),"invalid legacy route tags");
        } else {
            ensure!(document.is_object(),"scoped route tags require a document");
            let summary=summary.as_deref().context("scoped route source summary unavailable")?;
            let decoded:Value=encrypt::open_json_b64(&key,&aad::route_summary(&creds.user_id,&creds.pi_id,&source.id),summary)?;
            ensure!(decoded["file"].as_str()==Some(file.as_str()) && decoded["gr"]==serde_json::to_value(&source.runs)?,
                "scoped route frame source changed");
            let total=source.runs.chunks_exact(2).try_fold(0u32,|total,run| {
                ensure!(run[0]<=255 && run[1]>0,"invalid scoped frame source");
                total.checked_add(run[1]).context("scoped frame count overflow")
            })?.max(1);
            ensure!(document["totalFrames"].as_u64()==Some(u64::from(total)),"scoped frame count changed");
        }
        documents.insert(file.clone(),document);
        verified.push(VerifiedRouteSyncKey {file:file.clone(),route_id:source.id.clone(),expected_key:source.key.clone(),uploaded_at:source.uploaded_at,wrapped_key:wrapped_key.clone()});
    }
    let mut drives=Vec::new();
    for (drive,windows) in snapshot.drives {
        let mut tags=BTreeSet::new();
        for window in windows {
            let document=documents.get(&window.file).context("drive tag member not decoded")?;
            if let Some(legacy)=document.as_array() {
                tags.extend(legacy.iter().map(|tag|tag.as_str().unwrap().to_string()));
            } else {
                tags.extend(scoped_route_tags::tags_for_ranges(document,window.total_frames,&[(window.start_frame,window.end_frame)])?);
            }
        }
        drives.push((drive,tags.into_iter().collect()));
    }
    Ok(Projection {revision:snapshot.revision,sources:verified,drives})
}

pub(super) async fn prepare(state:&Arc<CloudStateInner>,client:&CloudClient,creds:&CloudCredentialsV1,
    pi_key:&[u8;32],changes:&[RouteChange])->Result<Option<Projection>> {
    if changes.is_empty() {return Ok(None)}
    let binding=revision::binding(creds)?;
    {let _guard=revision::current_pairing(state,&binding).await?;}
    let snapshot={
        let state=state.clone();let changes:Vec<_>=changes.iter().map(|change|change.route_id.clone()).collect();
        tokio::task::spawn_blocking(move || snapshot(&state,&changes))
            .await.context("prepare drive tag projection")??
    };
    let Some(snapshot)=snapshot else {return Ok(None)};
    let mut ids:Vec<_>=snapshot.sources.values().map(|source|source.id.clone()).collect();ids.sort();
    let states=read_states(state,client,creds,&binding,&ids).await?;
    Ok(Some(project(snapshot,states,creds,pi_key)?))
}

#[cfg(test)]
pub(super) mod tests;

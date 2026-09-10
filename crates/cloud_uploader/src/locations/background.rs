//! Bounded historical enrichment. Original uploads are never requeued/replaced.
use super::*;
use std::sync::Arc;
use sentryusb_cloud_crypto::credentials::CloudCredentialsV1;
use sentryusb_drives::schema;
use crate::{credentials_store::UnlockedCreds, state::CloudStateInner};

const PAGE_SIZE: usize = 20;
const DOWNLOADS_PER_PASS: usize = 4;
const RESCAN_SECONDS: i64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scan {
    version: u8,
    binding: String,
    #[serde(deserialize_with = "nullable")]
    before: Option<String>,
    resume_at: i64,
}
#[derive(Default)]
struct Pass { checked: usize, updated: usize, unavailable: usize, failed: usize, downloads: usize, wait_seconds: u64 }
struct Snapshot { route: Route, readings: Option<LocationReadings>, fingerprint: String }

// A new derived walk revisits summaries for the additional evidence field.
// Existing uncertain publication/cancellation receipts are still resolved first.
fn scan_key(binding: &str) -> String { format!("cloud_summary_scan_v3:{binding}") }
fn checked_key(binding: &str, id: &str) -> String { format!("cloud_summary_checked_v3:{binding}:{id}") }

fn load_scan(store: &DriveStore, binding: &str) -> Result<(Option<String>, Scan)> {
    let raw = store.with_read_conn(|conn| schema::meta_get(conn, &scan_key(binding)))?;
    let scan = match raw.as_deref() {
        None => Scan {version:1,binding:binding.into(),before:None,resume_at:0},
        Some(raw) => {
            ensure!(raw.len() <= 8192, "location scan checkpoint exceeds its limit");
            let scan: Scan = serde_json::from_str(raw).context("unreadable location scan checkpoint")?;
            ensure!(scan.version == 1 && scan.binding == binding && scan.resume_at >= 0
                && scan.before.as_ref().is_none_or(|file| !file.is_empty() && file.len() <= 4096), "invalid location scan checkpoint");
            scan
        }
    };
    Ok((raw, scan))
}
async fn save_scan(state: &CloudStateInner, creds: &CloudCredentialsV1, expected: Option<&str>, scan: &Scan) -> Result<()> {
    let _guard = state.current_credentials(creds).await?;
    state.store.with_durable_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        let key = scan_key(&scan.binding);
        ensure!(schema::meta_get(&tx, &key)?.as_deref() == expected, "location scan checkpoint changed");
        schema::meta_set(&tx, &key, &serde_json::to_string(scan)?)?;
        tx.commit()?; Ok(())
    })
}
fn candidates(store: &DriveStore, before: Option<&str>) -> Result<Vec<String>> {
    store.with_read_conn(|conn| {
        // A keyset walk cannot skip rows merely because another file is deleted.
        // Newer insertions are covered by normal uploads and the next full walk.
        let files = if let Some(before) = before {
            let mut stmt = conn.prepare_cached("SELECT file FROM routes WHERE cloud_uploaded_at > 0 AND file < ?1 ORDER BY file DESC LIMIT ?2")?;
            stmt.query_map(rusqlite::params![before, PAGE_SIZE as i64], |row| row.get::<_,String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let mut stmt = conn.prepare_cached("SELECT file FROM routes WHERE cloud_uploaded_at > 0 ORDER BY file DESC LIMIT ?1")?;
            stmt.query_map([PAGE_SIZE as i64], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        ensure!(files.iter().all(|file| !file.is_empty() && file.len() <= 4096), "invalid location scan filename");
        Ok(files)
    })
}
async fn snapshot(store: Arc<DriveStore>, file: String) -> Result<Option<Snapshot>> {
    tokio::task::spawn_blocking(move || store.with_route_snapshot(&file, |route, conn| {
        let Some(route) = route else {return Ok(None)};
        ensure!(route.file == file, "location source path changed");
        let (uploaded, id): (Option<i64>,Option<String>) = conn.query_row(
            "SELECT cloud_uploaded_at,cloud_route_id FROM routes WHERE file=?1", [&file], |row| Ok((row.get(0)?,row.get(1)?)))?;
        if !uploaded.is_some_and(|at| at > 0) {return Ok(None)}
        ensure!(id.is_none_or(|id| id == ids::route_id_from_path(&file)), "local Cloud route identity changed");
        let readings = for_route_on_conn(conn, &route)?;
        let fingerprint = evidence_fingerprint(&route, readings.as_ref())?;
        Ok(Some(Snapshot {route,readings,fingerprint}))
    })).await.context("location source reader stopped")?
}

// Decrypt only the small summary first. Already-complete summaries never need
// another original-blob download, even during the periodic restoration scan.
fn needs_original(local: &Snapshot, remote: &SourceIdentity, creds: &CloudCredentialsV1, pi_key: &[u8;32]) -> Result<bool> {
    let Some(ciphertext) = &remote.summary_ciphertext else { return Ok(true) };
    let mut key = crate::encrypt::unwrap_content_key(pi_key, &remote.wrapped_route_key,
        &aad::route_key(&creds.user_id, &creds.pi_id, &remote.route_id))?;
    let decoded: Result<Value> = crate::encrypt::open_json_b64(&key,
        &aad::route_summary(&creds.user_id, &creds.pi_id, &remote.route_id), ciphertext);
    key.fill(0);
    let summary = decoded?;
    ensure!(summary.is_object() && summary["file"].as_str() == Some(local.route.file.as_str())
        && summary["v"].as_u64().is_some_and(|v| (1..=4).contains(&v)), "unsupported or mismatched source summary");
    let mut with_evidence=summary.clone();
    // Local evidence only selects candidates. prepare_summary always copies
    // the authenticated original's runs, never these unverified local values.
    crate::summon_evidence::attach(&mut with_evidence,&local.route,&creds.user_id,&creds.pi_id,&remote.route_id,&remote.wrapped_route_key);
    Ok(with_evidence!=summary || summary.get("nm").is_none() || missing(summary.get("ls")) || missing(summary.get("le"))
        || (has_internal_park(&local.route) && summary.get("lr").is_none()))
}
fn checked_stamp(local: &Snapshot, remote: &SourceIdentity) -> Result<String> {
    let bytes = serde_json::to_vec(&(&local.fingerprint, &remote.route_id, &remote.record_version,
        &remote.wrapped_route_key, &remote.summary_ciphertext))?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref().iter().map(|byte|format!("{byte:02x}")).collect())
}
async fn retain_checked(state: &CloudStateInner, creds: &CloudCredentialsV1, key: &str, stamp: &str) -> Result<()> {
    let _guard = state.current_credentials(creds).await?;
    state.store.with_durable_conn(|conn| schema::meta_set(conn, key, stamp))
}

async fn advance_pending(state: &CloudStateInner, creds: &CloudCredentialsV1, token: &[u8],
    client: &impl SummaryTransport, raw: String, pending: PendingSummary) -> Result<bool> {
    // A failed local read is not evidence of deletion: retain the attempt and
    // retry the read. A confirmed missing row can safely request cancellation.
    let current = snapshot(state.store.clone(), pending.file.clone()).await?;
    let (outcome,cancel) = PendingSummary::advance(state,creds,token,client,raw,pending,
        current.as_ref().map(|current|current.fingerprint.as_str())).await?;
    Ok(outcome == SummaryOutcome::Applied && !cancel)
}

async fn run_pass(state: &CloudStateInner, creds: &CloudCredentialsV1, token: &[u8], pi_key: &[u8;32],
    client: &impl SummaryTransport, now: i64) -> Result<Pass> {
    ensure!((0..=253402300799).contains(&now), "invalid location scan clock");
    let _serial = state.location_sync.lock().await;
    { let _guard = state.current_credentials(creds).await?; }
    let mut pass = Pass::default();
    // Resolve uncertain publication before the scanner can create another one.
    if let Some((raw,pending)) = PendingSummary::load(&state.store,creds)? {
        pass.updated += usize::from(advance_pending(state,creds,token,client,raw,pending).await?);
    }
    let binding = pairing_binding(creds)?;
    let (raw,mut scan) = load_scan(&state.store,&binding)?;
    if scan.resume_at > now { pass.wait_seconds=(scan.resume_at-now) as u64;return Ok(pass) }
    let files = candidates(&state.store,scan.before.as_deref())?;
    if files.is_empty() {
        scan.before=None;scan.resume_at=now.saturating_add(RESCAN_SECONDS);pass.wait_seconds=RESCAN_SECONDS as u64;
        save_scan(state,creds,raw.as_deref(),&scan).await?;
        return Ok(pass);
    }
    let ids:Vec<_> = files.iter().map(|file|ids::route_id_from_path(file)).collect();
    let sources = client.states(creds,token,&ids).await?;
    { let _guard = state.current_credentials(creds).await?; }
    ensure!(sources.len() == ids.len() && ids.iter().all(|id|sources.contains_key(id)), "incomplete location source batch");
    for (file,id) in files.iter().zip(&ids) {
        if pass.downloads == DOWNLOADS_PER_PASS {break;}
        let result = async {
            let Some(remote) = sources.get(id).context("missing location source result")? else {
                pass.unavailable += 1;return Ok(());
            };
            ensure!(remote.route_id == *id, "location source identity mismatch");
            let Some(local) = snapshot(state.store.clone(),file.clone()).await? else {return Ok(())};
            if local.route.source.as_deref().is_some_and(|source|source != "sei") {return Ok(())}
            if !needs_original(&local,remote,creds,pi_key)? {return Ok(())}
            let key = checked_key(&binding,id);
            let stamp = checked_stamp(&local,remote)?;
            if state.store.with_read_conn(|conn|schema::meta_get(conn,&key))?.as_deref() == Some(&stamp) {return Ok(())}
            pass.downloads += 1;
            let (source,blob) = client.original(creds,token,remote).await?;
            { let _guard = state.current_credentials(creds).await?; }
            ensure!(source.route_id == remote.route_id && source.record_version == remote.record_version
                && source.wrapped_route_key == remote.wrapped_route_key && source.summary_ciphertext == remote.summary_ciphertext,
                "downloaded location source changed");
            let prepared = prepare_summary(&local.route,local.readings.as_ref(),&blob,&source,pi_key,&creds.user_id,&creds.pi_id)?;
            let Some(fresh) = snapshot(state.store.clone(),file.clone()).await? else {return Ok(())};
            ensure!(fresh.fingerprint == local.fingerprint, "location evidence changed during preparation");
            if let Some(prepared) = prepared {
                let (raw,pending) = PendingSummary::reserve(state,creds,file.clone(),prepared).await?;
                pass.updated += usize::from(advance_pending(state,creds,token,client,raw,pending).await?);
            } else {
                // This exact local evidence/source pair was checked against the
                // original. Changes to either invalidate the download cache.
                retain_checked(state,creds,&key,&stamp).await?;
            }
            Ok::<(),anyhow::Error>(())
        }.await;
        if result.is_err() {
            { let _guard = state.current_credentials(creds).await?; }
            // Never advance beyond an uncertain write. Pre-publication source
            // failures may move on; the next full scan retries those records.
            if PendingSummary::load(&state.store,creds)?.is_some() {return result.map(|_|pass)}
            pass.failed += 1;
        }
        pass.checked += 1;scan.before=Some(file.clone());
    }
    save_scan(state,creds,raw.as_deref(),&scan).await?;
    Ok(pass)
}

pub(crate) async fn run_loop(state: Arc<CloudStateInner>) {
    let client = match SummaryClient::new() {
        Ok(client) => client,
        Err(_) => { tracing::warn!("Cloud location repair client could not start");return; }
    };
    let mut delay = 30u64;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        let Some(creds) = state.creds.lock().await.clone() else {delay=60;continue;};
        let unlocked = match UnlockedCreds::unlock(&creds) {
            Ok(unlocked) => unlocked,
            Err(_) => {delay=300;continue;}
        };
        match run_pass(&state,&creds,&unlocked.pi_auth_token,&unlocked.pi_key,&client,chrono::Utc::now().timestamp()).await {
            Ok(pass) => {
                delay=pass.wait_seconds.clamp(15,900);
                if pass.updated > 0 || pass.failed > 0 {
                    tracing::info!(checked=pass.checked,updated=pass.updated,unavailable=pass.unavailable,failed=pass.failed,
                        "Cloud location repair pass");
                }
            }
            Err(_) => {
                // Normal upload/tag sync handles revoked credentials and rekeys.
                // Do not log source URLs, filenames or decrypted locations here.
                delay=(delay.max(30)*2).min(900);
                tracing::warn!("Cloud location repair paused; retry scheduled");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Mutex, atomic::{AtomicBool,AtomicUsize,Ordering}};

    #[derive(Default)]
    struct Fake {
        records: Mutex<HashMap<String,(SummarySource,Vec<u8>)>>,
        receipts: Mutex<HashMap<String,SummaryOutcome>>,
        requests: Mutex<Vec<(String,bool)>>,
        downloads: AtomicUsize,
        reads: Mutex<Vec<Vec<String>>>,
        lose_write_reply: AtomicBool,
        corrupt: Mutex<HashSet<String>>,
        on_download: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        on_states: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    impl SummaryTransport for Fake {
        async fn states(&self, _: &CloudCredentialsV1, _: &[u8], ids: &[String]) -> Result<HashMap<String,Option<SourceIdentity>>> {
            self.reads.lock().unwrap().push(ids.to_vec());
            let hook=self.on_states.lock().unwrap().take();if let Some(hook)=hook {hook();}
            let records=self.records.lock().unwrap();
            Ok(ids.iter().map(|id|(id.clone(),records.get(id).map(|(source,_)|SourceIdentity {
                route_id:source.route_id.clone(),record_version:source.record_version.clone(),wrapped_route_key:source.wrapped_route_key.clone(),
                summary_ciphertext:source.summary_ciphertext.clone()}))).collect())
        }
        async fn original(&self, _: &CloudCredentialsV1, _: &[u8], source: &SourceIdentity) -> Result<(SummarySource,Vec<u8>)> {
            self.downloads.fetch_add(1,Ordering::SeqCst);
            let hook=self.on_download.lock().unwrap().take();if let Some(hook)=hook {hook();}
            let mut result=self.records.lock().unwrap().get(&source.route_id).context("synthetic missing object")?.clone();
            if self.corrupt.lock().unwrap().contains(&source.route_id) {*result.1.last_mut().unwrap()^=1;}
            Ok(result)
        }
        async fn publish(&self, _: &CloudCredentialsV1, _: &[u8], proposal: &SummaryProposal, cancel: bool) -> Result<SummaryOutcome> {
            self.requests.lock().unwrap().push((serde_json::to_string(proposal)?,cancel));
            let mut receipts=self.receipts.lock().unwrap();
            let outcome=if let Some(outcome)=receipts.get(&proposal.operation_id) {*outcome} else {
                let mut records=self.records.lock().unwrap();
                let outcome=if cancel {SummaryOutcome::Cancelled}
                    else if let Some((source,_))=records.get_mut(&proposal.route_id) {
                        if source.record_version!=proposal.record_version || source.wrapped_route_key!=proposal.wrapped_route_key {SummaryOutcome::SourceChanged}
                        else if source.summary_ciphertext!=proposal.expected_summary_ciphertext {SummaryOutcome::Conflict}
                        else {source.summary_ciphertext=Some(proposal.summary_ciphertext.clone());SummaryOutcome::Applied}
                    } else {SummaryOutcome::NotFound};
                receipts.insert(proposal.operation_id.clone(),outcome);outcome
            };
            if self.lose_write_reply.swap(false,Ordering::SeqCst) {anyhow::bail!("synthetic lost response");}
            Ok(outcome)
        }
    }
    fn creds() -> CloudCredentialsV1 {
        CloudCredentialsV1 {version:1,user_id:"owner".into(),pi_id:"pi".into(),pi_auth_token:"synthetic".into(),wrapped_pi_key_local:"synthetic".into(),
            long_term_x25519:sentryusb_cloud_crypto::credentials::LongTermX25519OnDisk {public_key:"synthetic".into(),wrapped_private_key:"synthetic".into()},
            cloud_base_url:"http://127.0.0.1:1".into(),paired_at:chrono::Utc::now(),dek_rotation_generation:0}
    }
    async fn state(store: DriveStore, creds: &CloudCredentialsV1) -> Arc<CloudStateInner> {
        let state=Arc::new(CloudStateInner::new(Arc::new(store),sentryusb_ws::Hub::new(),Arc::new(tokio::sync::Notify::new()),
            creds.cloud_base_url.clone(),String::new(),None));
        *state.creds.lock().await=Some(creds.clone());state
    }
    fn seed(store: &DriveStore, fake: &Fake, n: usize, named: bool) -> String {
        seed_route(store,fake,n,named,false)
    }
    fn seed_route(store: &DriveStore, fake: &Fake, n: usize, named: bool, park: bool) -> String {
        use sentryusb_drives::types::GearRun;
        let file=format!("RecentClips/2026-09-07_12-{:02}-00-front.mp4",n);
        let gears:Vec<u8>=if park {(0..60).map(|i|if (10..30).contains(&i) {0} else {4}).collect()} else {vec![4,4]};
        let runs=if park {vec![GearRun {gear:4,frames:10},GearRun {gear:0,frames:20},GearRun {gear:4,frames:30}]}
            else {vec![GearRun {gear:4,frames:2}]};
        store.add_route(&file,"2026-09-07",&[[40.0,-73.0],[40.0001,-73.0001]],&gears,&[0,0],&[1.0,2.0],&[0.0,0.0],0,gears.len() as u32,
            &runs,&[]).unwrap();
        let original=store.with_routes_by_files(&[&file],|routes|routes[0].clone()).unwrap();
        let file=original.file.clone();
        crate::db_ext::mark_uploaded(store,&file,1).unwrap();
        let encrypted=crate::encrypt::encrypt_route(&original,&[9;32],"owner","pi",None).unwrap();
        let blob=B64.decode(encrypted.route_blob_b64).unwrap();
        let source=SummarySource {route_id:encrypted.route_id,record_version:"a".repeat(64),wrapped_route_key:encrypted.wrapped_route_key_b64,
            summary_ciphertext:Some(encrypted.summary_ciphertext_b64),blob_len:blob.len()};
        fake.records.lock().unwrap().insert(source.route_id.clone(),(source,blob));
        if named {set_name(store,&file,"Library");}
        file
    }
    fn set_name(store: &DriveStore, file: &str, name: &str) {
        store.with_locked_conn(|conn| -> Result<()> {
            conn.execute("UPDATE routes SET location_name_start=?2,location_name_end=?2 WHERE file=?1",rusqlite::params![file,name])?;
            Ok(())
        }).unwrap();
    }
    fn restart_scan(store: &DriveStore, creds: &CloudCredentialsV1) {
        store.with_durable_conn(|conn|schema::meta_del(conn,&scan_key(&pairing_binding(creds).unwrap()))).unwrap();
    }
    fn summary(fake: &Fake, file: &str) -> Value {
        let id=ids::route_id_from_path(file);let records=fake.records.lock().unwrap();let (source,_)=&records[&id];
        let key=crate::encrypt::unwrap_content_key(&[9;32],&source.wrapped_route_key,&aad::route_key("owner","pi",&id)).unwrap();
        crate::encrypt::open_json_b64(&key,&aad::route_summary("owner","pi",&id),source.summary_ciphertext.as_ref().unwrap()).unwrap()
    }
    #[tokio::test]
    async fn evidence_walk_restarts_without_rewriting_the_previous_checkpoint() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;
        let binding=pairing_binding(&creds).unwrap();let old_key=format!("cloud_summary_scan_v2:{binding}");
        let old=serde_json::json!({"version":1,"binding":binding,"before":"older-file.mp4","resume_at":999999}).to_string();
        state.store.with_durable_conn(|conn|schema::meta_set(conn,&old_key,&old)).unwrap();
        let (expected,new)=load_scan(&state.store,&binding).unwrap();
        assert!(expected.is_none());assert!(new.before.is_none());assert_eq!(new.resume_at,0);
        save_scan(&state,&creds,None,&new).await.unwrap();
        assert_eq!(state.store.with_read_conn(|conn|schema::meta_get(conn,&old_key)).unwrap(),Some(old));
        assert!(load_scan(&state.store,&binding).unwrap().0.is_some());
    }
    #[tokio::test]
    async fn scan_is_bounded_resumes_and_leaves_original_blobs_keys_and_local_rows_intact() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let files:Vec<_>=(0..6).map(|n|seed(&state.store,&fake,n,true)).collect();
        let original:HashMap<_,_>=fake.records.lock().unwrap().iter().map(|(id,(source,blob))|(id.clone(),(source.wrapped_route_key.clone(),blob.clone()))).collect();
        let local=state.store.get_routes().unwrap();
        let first=run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();
        assert_eq!((first.checked,first.updated,first.downloads),(4,4,4));
        assert_eq!(load_scan(&state.store,&pairing_binding(&creds).unwrap()).unwrap().1.before.as_deref(),Some(files[2].as_str()));
        let next=run_pass(&state,&creds,&[3;32],&[9;32],&fake,115).await.unwrap();assert_eq!((next.checked,next.updated),(2,2));
        let end=run_pass(&state,&creds,&[3;32],&[9;32],&fake,130).await.unwrap();assert_eq!(end.checked,0);
        assert_eq!(load_scan(&state.store,&pairing_binding(&creds).unwrap()).unwrap().1.resume_at,130+RESCAN_SECONDS);
        run_pass(&state,&creds,&[3;32],&[9;32],&fake,131).await.unwrap();assert_eq!(fake.downloads.load(Ordering::SeqCst),6);
        for file in &files {assert_eq!(summary(&fake,file)["ls"],"Library");assert_eq!(summary(&fake,file)["le"],"Library");}
        for (id,(source,blob)) in fake.records.lock().unwrap().iter() {assert_eq!((&source.wrapped_route_key,blob),(&original[id].0,&original[id].1));}
        assert_eq!(serde_json::to_value(state.store.get_routes().unwrap()).unwrap(),serde_json::to_value(local).unwrap());
        assert!(PendingSummary::load(&state.store,&creds).unwrap().is_none());
    }
    #[tokio::test]
    async fn missing_and_corrupt_sources_do_not_block_other_records_or_become_successful_repairs() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let good=seed(&state.store,&fake,0,true);let corrupt=seed(&state.store,&fake,1,true);let missing=seed(&state.store,&fake,2,true);
        fake.corrupt.lock().unwrap().insert(ids::route_id_from_path(&corrupt));fake.records.lock().unwrap().remove(&ids::route_id_from_path(&missing));
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();
        assert_eq!((pass.checked,pass.updated,pass.failed,pass.unavailable),(3,1,1,1));assert_eq!(summary(&fake,&good)["ls"],"Library");
        assert!(super::missing(summary(&fake,&corrupt).get("ls")));
        fake.corrupt.lock().unwrap().clear();restart_scan(&state.store,&creds);
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();assert_eq!(pass.updated,1);
    }
    #[tokio::test]
    async fn unchanged_unrepairable_evidence_skips_repeat_downloads_but_new_evidence_is_rechecked() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let file=seed(&state.store,&fake,0,false);
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();assert_eq!((pass.updated,pass.downloads),(0,1));
        restart_scan(&state.store,&creds);run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();assert_eq!(fake.downloads.load(Ordering::SeqCst),1);
        set_name(&state.store,&file,"Library");restart_scan(&state.store,&creds);
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,300).await.unwrap();assert_eq!((pass.updated,pass.downloads),(1,1));
    }
    #[tokio::test]
    async fn local_change_during_download_does_not_publish_or_cache_stale_evidence() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let file=seed(&state.store,&fake,0,true);let store=state.store.clone();let changed=file.clone();
        *fake.on_download.lock().unwrap()=Some(Box::new(move ||set_name(&store,&changed,"Office")));
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();assert_eq!((pass.failed,pass.updated),(1,0));
        assert!(fake.requests.lock().unwrap().is_empty());assert!(PendingSummary::load(&state.store,&creds).unwrap().is_none());
        restart_scan(&state.store,&creds);let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();
        assert_eq!(pass.updated,1);assert_eq!(summary(&fake,&file)["ls"],"Office");
    }
    #[tokio::test]
    async fn uncertain_publication_retains_scan_position_and_replays_before_new_downloads() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let file=seed(&state.store,&fake,0,true);fake.lose_write_reply.store(true,Ordering::SeqCst);
        assert!(run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.is_err());
        assert!(load_scan(&state.store,&pairing_binding(&creds).unwrap()).unwrap().0.is_none());
        assert!(PendingSummary::load(&state.store,&creds).unwrap().is_some());
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();assert_eq!(pass.updated,1);
        assert_eq!(fake.downloads.load(Ordering::SeqCst),1);assert!(PendingSummary::load(&state.store,&creds).unwrap().is_none());
        let requests=fake.requests.lock().unwrap();assert_eq!(requests.len(),2);assert_eq!(requests[0],requests[1]);assert_eq!(summary(&fake,&file)["ls"],"Library");
    }
    #[tokio::test]
    async fn pairing_replacement_discards_late_source_batch_without_advancing_old_scan() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();seed(&state.store,&fake,0,true);
        let changed=state.clone();*fake.on_states.lock().unwrap()=Some(Box::new(move ||changed.creds.try_lock().unwrap().as_mut().unwrap().dek_rotation_generation+=1));
        assert!(run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.is_err());
        assert_eq!(fake.downloads.load(Ordering::SeqCst),0);assert!(load_scan(&state.store,&pairing_binding(&creds).unwrap()).unwrap().0.is_none());
    }
    #[tokio::test]
    async fn file_backed_snapshot_does_not_mix_old_route_with_new_telemetry() {
        let creds=creds();let temp=tempfile::tempdir().unwrap();let path=temp.path().join("snapshot.sqlite");
        let state=state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;let fake=Fake::default();let file=seed(&state.store,&fake,0,true);
        let writer=state.store.clone();let changed=file.clone();
        state.store.with_route_snapshot(&file,|route,conn| {
            assert_eq!(route.unwrap().location_name_start.as_deref(),Some("Library"));
            std::thread::spawn(move ||writer.with_locked_conn(|conn| -> Result<()> {
                let tx=conn.unchecked_transaction()?;
                tx.execute("UPDATE routes SET location_name_start='Office' WHERE file=?1",[changed])?;
                tx.execute("INSERT INTO telemetry_samples (ts,source,location_name) VALUES (123,'sei','Office')",[])?;
                tx.commit()?;Ok(())
            }).unwrap()).join().unwrap();
            assert_eq!(conn.query_row("SELECT count(*) FROM telemetry_samples WHERE ts=123",[],|row|row.get::<_,i64>(0))?,0);
            Ok(())
        }).unwrap();
        let fresh=snapshot(state.store.clone(),file).await.unwrap().unwrap();assert_eq!(fresh.route.location_name_start.as_deref(),Some("Office"));
    }
    #[tokio::test]
    async fn internal_park_enrichment_uses_timestamped_evidence_through_the_actual_scan() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();
        let file=seed_route(&state.store,&fake,0,true,true);
        let clock=sentryusb_drives::grouper::parse_clip_timestamp(&file).unwrap();let start=Local.from_local_datetime(&clock).single().unwrap().timestamp();
        state.store.with_locked_conn(|conn|conn.execute("INSERT INTO telemetry_samples (ts,source,latitude,longitude,location_name) VALUES (?1,'sei',40.0,-73.0,'Stop')",[start+20])).unwrap();
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();assert_eq!(pass.updated,1);
        let after=summary(&fake,&file);assert_eq!(after["lr"],serde_json::json!({"v":1,"readings":[[20000,40.0,-73.0,"Stop"]]}));
        restart_scan(&state.store,&creds);run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();
        assert_eq!(fake.downloads.load(Ordering::SeqCst),1);
    }
    #[tokio::test]
    async fn checkpoint_reopen_and_keyset_walk_handle_deletion_and_newer_insertions() {
        let creds=creds();let temp=tempfile::tempdir().unwrap();let path=temp.path().join("scan.sqlite");
        let current=state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;let fake=Fake::default();
        let files:Vec<_>=(0..41).map(|n|seed(&current.store,&fake,n,true)).collect();fake.records.lock().unwrap().clear();
        let pass=run_pass(&current,&creds,&[3;32],&[9;32],&fake,100).await.unwrap();assert_eq!(pass.checked,20);
        current.store.with_locked_conn(|conn|conn.execute("DELETE FROM routes WHERE file=?1",[&files[40]])).unwrap();
        drop(current);
        let current=state(crate::open_test_store(path.to_str().unwrap()).unwrap(),&creds).await;
        let added=seed(&current.store,&fake,59,true);fake.records.lock().unwrap().clear();
        let pass=run_pass(&current,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();assert_eq!(pass.checked,20);
        let pass=run_pass(&current,&creds,&[3;32],&[9;32],&fake,300).await.unwrap();assert_eq!(pass.checked,1);
        let visited:Vec<_>=fake.reads.lock().unwrap().iter().flatten().cloned().collect();
        assert_eq!(visited,files.iter().rev().map(|file|ids::route_id_from_path(file)).collect::<Vec<_>>());
        run_pass(&current,&creds,&[3;32],&[9;32],&fake,400).await.unwrap();
        run_pass(&current,&creds,&[3;32],&[9;32],&fake,400+RESCAN_SECONDS).await.unwrap();
        assert_eq!(fake.reads.lock().unwrap().last().unwrap()[0],ids::route_id_from_path(&added));
        let plan=current.store.with_read_conn(|conn| -> Result<Vec<String>> {
            let mut stmt=conn.prepare("EXPLAIN QUERY PLAN SELECT file FROM routes WHERE cloud_uploaded_at > 0 AND file < ?1 ORDER BY file DESC LIMIT ?2")?;
            Ok(stmt.query_map(rusqlite::params![&files[20],20],|row|row.get(3))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).unwrap();assert!(plan.iter().any(|line|line.contains("SEARCH routes") && line.contains("file<?")),"{plan:?}");
    }
    #[tokio::test]
    async fn failed_scan_checkpoint_retries_without_republishing_confirmed_summaries() {
        let creds=creds();let state=state(DriveStore::open_memory().unwrap(),&creds).await;let fake=Fake::default();seed(&state.store,&fake,0,true);
        state.store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_scan_checkpoint BEFORE INSERT ON meta WHEN NEW.key LIKE 'cloud_summary_scan_v3:%' BEGIN SELECT RAISE(ABORT,'synthetic checkpoint failure'); END;")).unwrap();
        assert!(run_pass(&state,&creds,&[3;32],&[9;32],&fake,100).await.is_err());
        assert!(load_scan(&state.store,&pairing_binding(&creds).unwrap()).unwrap().0.is_none());
        assert_eq!(fake.requests.lock().unwrap().len(),1);
        state.store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_scan_checkpoint")).unwrap();
        let pass=run_pass(&state,&creds,&[3;32],&[9;32],&fake,200).await.unwrap();assert_eq!((pass.checked,pass.updated),(1,0));
        assert_eq!(fake.requests.lock().unwrap().len(),1);assert_eq!(fake.downloads.load(Ordering::SeqCst),1);
    }

}

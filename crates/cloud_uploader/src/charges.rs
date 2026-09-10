//! Uploads completed charge sessions derived with the same grouping rules as
//! the local charging API. Open sessions remain local until grouping is final.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use sentryusb_drives::charging::{
    self, ChargeSessionSummary, SESSION_GAP_SECS,
};
use sentryusb_drives::schema::{
    self, CHARGE_SWEEP_CURSOR_KEY, CHARGE_SWEEP_FULL_DATE_KEY,
};

use crate::client::CloudClient;
use crate::credentials_store::UnlockedCreds;
use crate::encrypt::{self, ChargeMutable, CostOverride};
use crate::state::{now_ms, CloudStateInner};

/// Maximum downsampled curve points per uploaded session.
const MAX_BLOB_POINTS: usize = 200;

const BATCH_LIMIT: usize = 32;

/// `uploaded_at` sentinel for sessions rejected as too large.
pub const PERMANENT_SKIP_SENTINEL: i64 = -1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadCharge {
    charge_id: String,
    charge_blob: String,
    wrapped_charge_key: String,
    summary_ciphertext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutable_ciphertext: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadBody {
    pi_id: String,
    charges: Vec<UploadCharge>,
}

#[derive(Deserialize)]
struct UploadResponse {
    results: Vec<UploadResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadResult {
    charge_id: String,
    status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentryusb_drives::charging::ChargeRow;

    const NOW: i64 = 1_000_000;

    fn session(start: i64, len: i64) -> Vec<ChargeRow> {
        (0..len)
            .map(|i| ChargeRow { ts: start + i * 60, ..Default::default() })
            .collect()
    }

    fn old(start: i64) -> Vec<ChargeRow> {
        session(start, 3)
    }

    #[test]
    fn settled_history_advances_to_the_newest_session() {
        let sessions = vec![old(1_000), old(50_000), old(100_000)];
        assert_eq!(
            sweep_frontier(&sessions, NOW, |_| true),
            Some(100_000),
            "everything sent and closed — start at the newest session"
        );
    }

    #[test]
    fn holds_at_the_oldest_unsent_session() {
        let sessions = vec![old(1_000), old(50_000), old(100_000)];
        // The cursor must not pass a failed upload.
        assert_eq!(
            sweep_frontier(&sessions, NOW, |ts| ts != 50_000),
            Some(50_000)
        );
    }

    #[test]
    fn holds_at_a_still_open_session() {
        let mut sessions = vec![old(1_000)];
        sessions.push(session(NOW - 60, 2));
        assert_eq!(sweep_frontier(&sessions, NOW, |_| true), Some(NOW - 60));
    }

    #[test]
    fn no_sessions_leaves_the_cursor_alone() {
        assert_eq!(sweep_frontier(&[], NOW, |_| true), None);
    }

    #[test]
    fn frontier_is_always_a_session_start() {
        let sessions = vec![old(1_000), old(50_000)];
        let f = sweep_frontier(&sessions, NOW, |ts| ts != 50_000).unwrap();
        assert!(sessions.iter().any(|s| s[0].ts == f));
    }
}

/// Returns the oldest open or unsent session start, or the newest start when settled.
fn sweep_frontier(
    sessions: &[Vec<charging::ChargeRow>],
    now_secs: i64,
    handled: impl Fn(i64) -> bool,
) -> Option<i64> {
    let settled = |s: &Vec<charging::ChargeRow>| {
        let closed = s.last().is_some_and(|l| now_secs - l.ts > SESSION_GAP_SECS);
        closed && s.first().is_some_and(|f| handled(f.ts))
    };
    sessions
        .iter()
        .filter(|s| !s.is_empty())
        .find(|s| !settled(s))
        .or_else(|| sessions.iter().rfind(|s| !s.is_empty()))
        .and_then(|s| s.first())
        .map(|r| r.ts)
}

async fn read_home(state:&CloudStateInner)->Result<Option<Option<sentryusb_drives::home::HomeGeofence>>> {
    let access=state.rate_config.clone();
    tokio::task::spawn_blocking(move||access.map(|access|access.home_geofence()).transpose())
        .await.context("read Home configuration task")?
}
fn upload_mutable(snapshot:&sentryusb_drives::db::ChargeUploadMutableSnapshot,summary:&ChargeSessionSummary,
    home:Option<Option<sentryusb_drives::home::HomeGeofence>>)->Option<ChargeMutable> {
    let at_home=home.map(|home|home.is_some_and(|home|home.contains(summary.location_lat,summary.location_lon)));
    if snapshot.tags.is_empty() && snapshot.cost.is_none() && at_home.is_none() {return None}
    Some(ChargeMutable {tags:snapshot.tags.clone(),cost_override:snapshot.cost.clone().map(|(amount,currency)|CostOverride {amount,currency}),at_home})
}

/// One sweep pass. Returns the number of sessions newly stored.
pub async fn sweep_once(state: Arc<CloudStateInner>) -> Result<u32> {
    let creds_snapshot = {
        let g = state.creds.lock().await;
        match g.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(0),
        }
    };
    let unlocked = UnlockedCreds::unlock(&creds_snapshot).or_else(|_| {
        let serial = std::env::var("SENTRYCLOUD_DEV_SERIAL")
            .map(|s| s.into_bytes())
            .map_err(|_| anyhow!("unlock failed and SENTRYCLOUD_DEV_SERIAL unset"))?;
        UnlockedCreds::unlock_with_serial(&creds_snapshot, &serial)
    })?;

    // Derivation is synchronous DB/CPU work; keep it off the async runtime.
    let store = state.store.clone();
    let home=read_home(&state).await?;
    let pending = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            // The raw cursor is a CAS token so imports cannot be overwritten
            // with a stale frontier mid-sweep.
            let (from, full_scan, cursor_token) = store.with_read_conn(|conn| -> Result<_> {
                let token = schema::meta_get(conn, CHARGE_SWEEP_CURSOR_KEY)?;
                if schema::meta_get(conn, CHARGE_SWEEP_FULL_DATE_KEY)?.as_deref()
                    != Some(today.as_str())
                {
                    return Ok((0, true, token));
                }
                let cursor = token
                    .as_deref()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                Ok((cursor, false, token))
            })?;

            // Uploads before `from` cannot match sessions in scope.
            let uploads = store
                .charge_uploads_map_since(from)
                .context("charge_uploads_map")?;
            let rows = store
                .with_read_conn(|conn| -> Result<_> { charging::load_charge_rows(conn, from, None) })
                .context("load charge rows")?;
            let now_secs = now_ms() / 1000;
            let sessions = charging::group_sessions(rows);

            // `>` matches `group_sessions` continuation at the exact gap.
            let closed = |s: &Vec<charging::ChargeRow>| {
                s.last().is_some_and(|l| now_secs - l.ts > SESSION_GAP_SECS)
            };
            let handled = |s: &Vec<charging::ChargeRow>| {
                s.first().is_some_and(|f| uploads.contains_key(&f.ts))
            };

            // Compute before uploads so the frontier remains conservative.
            let frontier = sweep_frontier(&sessions, now_secs, |ts| uploads.contains_key(&ts));
            store.with_locked_conn(|conn| -> Result<()> {
                if schema::meta_get(conn, CHARGE_SWEEP_CURSOR_KEY)? != cursor_token {
                    // Preserve an import's reset and force the next full rescan.
                    return Ok(());
                }
                if let Some(ts) = frontier {
                    schema::meta_set(conn, CHARGE_SWEEP_CURSOR_KEY, &ts.to_string())?;
                }
                if full_scan {
                    schema::meta_set(conn, CHARGE_SWEEP_FULL_DATE_KEY, &today)?;
                }
                Ok(())
            })?;

            // Outbox exclusion is only a POST gate; treating it as settled could
            // advance past a re-imported session.
            let outboxed: std::collections::HashSet<i64> = store
                .charge_delete_outbox_all()
                .context("charge delete outbox")?
                .into_iter()
                .map(|(ts, _, _)| ts)
                .collect();
            let pending: Vec<Vec<charging::ChargeRow>> = sessions
                .into_iter()
                .filter(|s| {
                    closed(s)
                        && !handled(s)
                        && !s.first().is_some_and(|f| outboxed.contains(&f.ts))
                })
                .collect();
            Ok(pending)
        })
        .await
        .map_err(|e| anyhow!("charge prep task: {}", e))??
    };
    if pending.is_empty() {
        return Ok(0);
    }

    // Values and their queue generation share a SQLite snapshot per session.
    let mut mutable_snapshots=std::collections::HashMap::new();
    for session in &pending {
        if let Some(first)=session.first() {
            mutable_snapshots.insert(first.ts,store.charge_mutable_for_upload(first.ts).context("read charge upload mutable")?);
        }
    }

    let client =
        CloudClient::new(&creds_snapshot.cloud_base_url).with_bearer(&unlocked.pi_auth_token);

    let mut total_stored: u32 = 0;
    for batch in pending.chunks(BATCH_LIMIT) {
        let mut wire = Vec::with_capacity(batch.len());
        // charge_id → (session_ts, wrapped key b64) for the ack loop.
        let mut by_id = std::collections::HashMap::new();
        for session in batch {
            let summary: ChargeSessionSummary = charging::summarize(session);
            let points =
                charging::downsample_points(charging::session_points(session), MAX_BLOB_POINTS);
            let mutable_snapshot=mutable_snapshots.get(&summary.id).context("missing prepared charge mutable")?;
            let mutable=upload_mutable(mutable_snapshot,&summary,home);
            let enc = encrypt::encrypt_charge(
                &summary,
                &points,
                mutable.as_ref(),
                &unlocked.pi_key,
                &creds_snapshot.user_id,
                &creds_snapshot.pi_id,
            )
            .with_context(|| format!("encrypt charge {}", summary.id))?;
            by_id.insert(
                enc.charge_id.clone(),
                (summary.id, enc.wrapped_charge_key_b64.clone()),
            );
            wire.push(UploadCharge {
                charge_id: enc.charge_id,
                charge_blob: enc.charge_blob_b64,
                wrapped_charge_key: enc.wrapped_charge_key_b64,
                summary_ciphertext: enc.summary_ciphertext_b64,
                mutable_ciphertext: enc.mutable_ciphertext_b64,
            });
        }

        let body = UploadBody {
            pi_id: creds_snapshot.pi_id.clone(),
            charges: wire,
        };
        anyhow::ensure!(read_home(&state).await?==home,"Home configuration changed; retry charging upload");
        { let _guard=state.current_credentials(&creds_snapshot).await?; }
        let resp = client
            .post_json_bearer("/api/pi/charges", &body)
            .await
            .map_err(|e| anyhow!("charge upload POST: {}", e))?;
        let status = resp.status();

        if status.as_u16() == 401 {
            warn!("charge upload: 401, wiping credentials");
            state.handle_remote_revoke(&creds_snapshot).await;
            return Err(anyhow!("auth rejected; pi unpaired"));
        }
        if status.as_u16() == 403 {
            let body_text = resp.text().await.unwrap_or_default();
            if body_text.contains("user_suspended") {
                *state.last_upload_error.lock().await = Some("user_suspended".to_string());
                return Err(anyhow!("user_suspended; charge uploads paused"));
            }
            warn!("charge upload: 403, wiping credentials");
            state.handle_remote_revoke(&creds_snapshot).await;
            return Err(anyhow!("auth rejected; pi unpaired"));
        }
        if status.as_u16() == 409 {
            let body_text = resp.text().await.unwrap_or_default();
            if body_text.contains("pi_key_stale") {
                // The route sweep owns rekey polling; retry charges afterward.
                return Err(anyhow!("pi_key_stale; awaiting rekey"));
            }
            return Err(anyhow!("charge upload: HTTP 409 body={}", body_text));
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("charge upload: HTTP {} body={}", status, body_text));
        }

        let parsed: UploadResponse = resp.json().await.context("parse charge upload response")?;
        let _pairing_guard=state.current_credentials(&creds_snapshot).await?;
        let outcome = acknowledge_batch(&store, &parsed, &by_id, &mutable_snapshots, now_ms() / 1000)?;
        total_stored += outcome.newly_stored;
        drop(_pairing_guard);

        if total_stored > 0 {
            state.hub.broadcast(
                "cloud_charge_upload",
                &serde_json::json!({ "uploaded": total_stored }),
            );
        }
        if outcome.consent_required {
            // Keep sessions queued until v2 consent is accepted.
            *state.last_upload_error.lock().await = Some("charge_consent_required".to_string());
            info!("charge upload: consent_required; pausing charge sweep");
            break;
        }
        if outcome.storage_full {
            *state.last_upload_error.lock().await = Some("storage_full".to_string());
            break;
        }
    }

    Ok(total_stored)
}

#[derive(Default)]
struct UploadOutcome {
    newly_stored: u32,
    consent_required: bool,
    storage_full: bool,
}

fn acknowledge_batch(
    store: &sentryusb_drives::DriveStore,
    parsed: &UploadResponse,
    by_id: &std::collections::HashMap<String, (i64, String)>,
    mutable_snapshots: &std::collections::HashMap<i64, sentryusb_drives::db::ChargeUploadMutableSnapshot>,
    now_unix: i64,
) -> Result<UploadOutcome> {
    let mut newly_stored = 0;
    let sent:std::collections::HashSet<_>=by_id.keys().map(String::as_str).collect();
    let mut seen=std::collections::HashSet::new();
    anyhow::ensure!(parsed.results.len()==sent.len() && parsed.results.iter().all(|result|
        sent.contains(result.charge_id.as_str()) && seen.insert(result.charge_id.as_str())
        && matches!(result.status.as_str(),"stored"|"duplicate"|"rejected_too_large"|"rejected_storage_full"|"rejected_consent_required")),
        "invalid charge upload acknowledgement");
    let mut consent_required = false;
    let mut storage_full = false;
    for result in &parsed.results {
        let Some((session_ts, wrapped_b64)) = by_id.get(&result.charge_id) else {
            continue;
        };
        match result.status.as_str() {
            "stored" | "duplicate" => {
                if result.status == "stored" {
                    newly_stored += 1;
                }
                let stored_key=if result.status=="stored" {Some(wrapped_b64.as_str())} else {None};
                let included=mutable_snapshots.get(session_ts).context("missing upload acknowledgement snapshot")?.changed_at;
                store.confirm_charge_upload(*session_ts,&result.charge_id,stored_key,now_unix,included)
                    .context("confirm charge upload")?;
            }
            "rejected_too_large" => {
                warn!("charge upload: rejected_too_large for {} (permanent skip)", session_ts);
                if let Err(e) = store.charge_upload_mark(
                    *session_ts,
                    &result.charge_id,
                    wrapped_b64,
                    PERMANENT_SKIP_SENTINEL,
                ) {
                    warn!("charge_upload_mark(skip) failed for {}: {}", session_ts, e);
                }
            }
            "rejected_storage_full" => storage_full = true,
            "rejected_consent_required" => consent_required = true,
            other => warn!("charge upload: unexpected status `{}`", other),
        }
    }
    Ok(UploadOutcome { newly_stored, consent_required, storage_full })
}

#[cfg(test)]
mod acknowledgement_tests {
    use super::*;
    use sentryusb_drives::DriveStore;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn stored_and_duplicate_replies_have_different_key_and_intent_outcomes() {
        for status in ["stored", "duplicate"] {
            let store = DriveStore::open_memory().unwrap();
            store.set_charge_tags(1, &["Work".into()]).unwrap();
            let snapshots = HashMap::from([(1, store.charge_mutable_for_upload(1).unwrap())]);
            let sent = HashMap::from([("synthetic-charge".into(), (1, "synthetic-new-key".into()))]);
            let response: UploadResponse = serde_json::from_value(json!({"results": [
                {"chargeId": "synthetic-charge", "status": status}
            ]})).unwrap();
            let result = acknowledge_batch(&store, &response, &sent, &snapshots, 100).unwrap();
            let uploads = store.charge_uploads_map_since(0).unwrap();
            let (_, key, uploaded_at) = &uploads[&1];
            assert_eq!(*uploaded_at, 100);
            assert_eq!(key, if status == "stored" { "synthetic-new-key" } else { "" });
            assert_eq!(store.dirty_mutables().unwrap().is_empty(), status == "stored");
            assert_eq!(store.get_charge_tags(1).unwrap(), vec!["Work"]);
            assert_eq!(result.newly_stored, u32::from(status == "stored"));
        }
    }

    #[test]
    fn invalid_acknowledgements_do_not_partially_change_local_uploads() {
        for results in [
            json!([{ "chargeId": "one", "status": "stored" }]),
            json!([{ "chargeId": "one", "status": "stored" }, { "chargeId": "one", "status": "duplicate" }]),
            json!([{ "chargeId": "one", "status": "stored" }, { "chargeId": "foreign", "status": "stored" }]),
            json!([{ "chargeId": "one", "status": "stored" }, { "chargeId": "two", "status": "unknown" }]),
        ] {
            let store = DriveStore::open_memory().unwrap();
            store.set_charge_tags(1, &["Work".into()]).unwrap();
            let snapshots = HashMap::from([(1, store.charge_mutable_for_upload(1).unwrap()),
                (2, store.charge_mutable_for_upload(2).unwrap())]);
            let sent = HashMap::from([("one".into(), (1, "new-one".into())), ("two".into(), (2, "new-two".into()))]);
            let response = serde_json::from_value(json!({ "results": results })).unwrap();
            assert!(acknowledge_batch(&store, &response, &sent, &snapshots, 100).is_err());
            assert!(store.charge_uploads_map_since(0).unwrap().is_empty());
            assert_eq!(store.dirty_mutables().unwrap().len(), 1);
            assert_eq!(store.get_charge_tags(1).unwrap(), vec!["Work"]);
        }
    }
}

#[cfg(test)]
mod home_upload_tests {
    use super::*;
    use sentryusb_drives::{db::ChargeUploadMutableSnapshot,home::HomeGeofence};
    #[test]
    fn initial_upload_keeps_derived_home_separate_from_user_tags_and_cost() {
        let summary=charging::summarize(&[charging::ChargeRow {ts:1,lat:Some(0.0),lon:Some(0.0),..Default::default()}]);
        let snapshot=ChargeUploadMutableSnapshot {tags:vec!["Work".into()],cost:Some((4.2,"CAD".into())),changed_at:Some(1)};
        let mutable=upload_mutable(&snapshot,&summary,Some(Some(HomeGeofence::new(0.0,0.0,120.0).unwrap()))).unwrap();
        assert_eq!(mutable.at_home,Some(true));assert_eq!(mutable.tags,vec!["Work".to_string()]);assert_eq!(mutable.cost_override.unwrap().amount,4.2);
        assert_eq!(upload_mutable(&snapshot,&summary,Some(Some(HomeGeofence::new(1.0,1.0,120.0).unwrap()))).unwrap().at_home,Some(false));
    }
    #[test]
    fn explicit_no_home_still_supplies_metadata_but_missing_provider_does_not_guess() {
        let summary=charging::summarize(&[charging::ChargeRow {ts:1,..Default::default()}]);
        let snapshot=ChargeUploadMutableSnapshot {tags:vec![],cost:None,changed_at:None};
        assert_eq!(upload_mutable(&snapshot,&summary,Some(None)).unwrap().at_home,Some(false));
        assert!(upload_mutable(&snapshot,&summary,None).is_none());
    }
}

#[cfg(test)]
mod failed_home_read_tests {
    use super::*;
    struct BrokenHome;
    impl crate::state::RateConfigAccess for BrokenHome {
        fn home_geofence(&self)->Result<Option<sentryusb_drives::home::HomeGeofence>> {anyhow::bail!("synthetic config read failure")}
        fn load_doc(&self,_:&sentryusb_drives::DriveStore)->Result<serde_json::Value> {Ok(serde_json::json!({}))}
        fn store_doc(&self,_:&serde_json::Value,_:&sentryusb_drives::DriveStore,_:i64)->Result<()> {Ok(())}
    }
    #[tokio::test]
    async fn unavailable_configuration_cannot_become_a_no_home_upload() {
        let store=Arc::new(sentryusb_drives::DriveStore::open_memory().unwrap());
        let mut state=CloudStateInner::new(store,sentryusb_ws::Hub::new(),Arc::new(tokio::sync::Notify::new()),
            "http://127.0.0.1:1".into(),String::new(),None);
        state.rate_config=Some(Arc::new(BrokenHome));
        assert!(read_home(&state).await.is_err());
        state.rate_config=None;assert_eq!(read_home(&state).await.unwrap(),None);
    }
}

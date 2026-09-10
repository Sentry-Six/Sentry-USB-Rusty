//! Mutable drive, charging and rate synchronization.
//! All publication merges field intent conditionally. Revision pulls use
//! no-echo setters and retain failed identities for targeted retries.
//! Object deletions do not propagate.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use sentryusb_cloud_crypto::aad;

use crate::client::CloudClient;
use crate::credentials_store::UnlockedCreds;
use crate::encrypt::{self, ChargeMutable};
use crate::state::CloudStateInner;

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "sync_push_tests.rs"]
mod push_tests;

mod revision;
mod incoming_retry;
mod incoming_apply;
mod merge_intent;
mod scoped_route_tags;
mod scoped_capability;
mod route_pull;
mod conditional_charge;
mod conditional_drive;
mod conditional_rate;
mod home;
pub(crate) fn work_pending(error:&anyhow::Error)->bool {home::is_pending(error)}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangesResponse {
    routes: Vec<RouteChange>,
    charges: Vec<ChargeChange>,
    rate_config: Option<RateConfigChange>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteChange {
    // The feed signals affected sources. Complete drive tags are read afresh
    // from every member so one clip cannot replace the whole drive's union.
    route_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChargeChange {
    charge_id: String,
    mutable_ciphertext: Option<String>,
    wrapped_charge_key: String,
    updated_at_ms: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateConfigChange {
    ciphertext: Option<String>,
    updated_at_ms: i64,
}

/// Pushes local changes, then pulls cloud changes; failures retry next sweep.
pub async fn run_once(state: Arc<CloudStateInner>) -> Result<()> {
    let creds_snapshot = {
        let g = state.creds.lock().await;
        match g.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        }
    };
    let ticket=state.begin_sync(&creds_snapshot).await?;
    let unlocked = UnlockedCreds::unlock(&creds_snapshot).or_else(|_| {
        let serial = std::env::var("SENTRYCLOUD_DEV_SERIAL")
            .map(|s| s.into_bytes())
            .map_err(|_| anyhow!("unlock failed and SENTRYCLOUD_DEV_SERIAL unset"))?;
        UnlockedCreds::unlock_with_serial(&creds_snapshot, &serial)
    });
    let unlocked=match unlocked {
        Ok(unlocked)=>unlocked,
        Err(error)=>{
            let _=state.finish_sync(&creds_snapshot,ticket,vec![crate::state::SyncStage::Credentials]).await;
            return Err(error);
        }
    };
    let client =
        CloudClient::new(&creds_snapshot.cloud_base_url).with_bearer(&unlocked.pi_auth_token);

    let drive_push = async {
        scoped_capability::register(&state, &client, &creds_snapshot).await?;
        conditional_drive::push(&state, &client, &creds_snapshot, &unlocked.pi_key).await
    }.await.context("drive tag sync push");
    // Give newly preserved Home-label pricing a chance to arrive before its
    // charge tags. Independent edits still run if one rate needs reconciliation.
    let pushed = conditional_rate::push(&state, &client, &creds_snapshot, &unlocked.pi_key).await.context("rate sync push");
    let charge_push = conditional_charge::push(&state, &client, &creds_snapshot, &unlocked.pi_key).await.context("charging sync push");
    // Reconcile readable Cloud changes even when one outgoing edit failed.
    let pulled = revision::pull_changes(&state, &client, &creds_snapshot, &unlocked.pi_key).await.context("sync pull");
    let home_config=conditional_rate::push_home_config(&state,&client,&creds_snapshot,&unlocked.pi_key).await.context("Home setting sync");
    let home_sync=home::push(&state,&client,&creds_snapshot,&unlocked.pi_key).await.context("Home charging sync");
    let home_pending=home_sync.as_ref().is_err_and(home::is_pending);
    let failures=[(crate::state::SyncStage::DriveTags,drive_push.is_err()),
        (crate::state::SyncStage::Charging,charge_push.is_err()),
        (crate::state::SyncStage::Rates,pushed.is_err()),
        (crate::state::SyncStage::Incoming,pulled.is_err()),
        (crate::state::SyncStage::Home,home_config.is_err()||(home_sync.is_err()&&!home_pending))].into_iter()
        .filter_map(|(stage,failed)|failed.then_some(stage)).collect();
    let _=state.finish_sync_with_home(&creds_snapshot,ticket,failures,home_pending).await;
    drive_push?;
    charge_push?;
    pushed?;
    pulled?;
    home_config?;
    home_sync?;
    Ok(())
}

// Apply decoded non-route records. The revision reader separately retains
// failed identities before publishing progress; retries fetch current state.
fn apply_change_page(
    state: &Arc<CloudStateInner>,
    user_id: &str,
    pi_id: &str,
    pi_key: &[u8; 32],
    parsed: &ChangesResponse,
) -> Result<()> {
    anyhow::ensure!(parsed.routes.is_empty(), "route tags require complete source projection");
    let store = state.store.clone();
    // Newer local dirty rows win and are pushed under server-side LWW.
    let dirty: HashMap<(String, String), i64> = store
        .dirty_mutables()?
        .into_iter()
        .map(|(kind, key, at)| ((kind, key), at))
        .collect();

    let has_changes = !parsed.routes.is_empty()
        || !parsed.charges.is_empty()
        || parsed.rate_config.is_some();
    if has_changes {
        for cc in &parsed.charges {
            let Some(session_ts) = store.charge_session_ts_for_cloud_id(&cc.charge_id)?
            else {
                continue; // never uploaded from this Pi / locally deleted
            };
            if dirty.contains_key(&("charge".to_string(), session_ts.to_string())) {
                // The conditional writer reconciles pending field operations.
                // A newer Cloud timestamp cannot discard unrelated local intent.
                continue;
            }
            let charge_key=encrypt::unwrap_content_key(pi_key,&cc.wrapped_charge_key,
                &aad::charge_key(user_id,pi_id,&cc.charge_id)).context("sync pull: unwrap chargeKey")?;
            let mutable: ChargeMutable = match &cc.mutable_ciphertext {
                None => ChargeMutable { at_home:None, tags: Vec::new(), cost_override: None },
                Some(ct) => {
                    match encrypt::open_json_b64(
                        &charge_key,
                        &aad::charge_mutable(user_id, pi_id, &cc.charge_id),
                        ct,
                    ) {
                        Ok(m) => m,
                        Err(e) => {
                            return Err(e).context("sync pull: open mutable");
                        }
                    }
                }
            };
            // Reserved tags are derived on read and must never be stored,
            // including values synced from older peers.
            let synced_tags: Vec<String> =
                sentryusb_drives::charging::strip_reserved_tags(mutable.tags.clone());
            let cost = mutable.cost_override.map(|c| (c.amount, c.currency));
            store.apply_charge_mutable_from_sync(session_ts, &cc.charge_id,
                &cc.wrapped_charge_key, &synced_tags, cost, cc.updated_at_ms)
                .context("sync pull: save charge mutable")?;
        }

        if let Some(rcfg) = &parsed.rate_config {
            if let (Some(ct), Some(access)) = (&rcfg.ciphertext, state.rate_config.as_ref()) {
                if dirty
                    .get(&("rate".to_string(), String::new()))
                    .is_some_and(|at| *at > rcfg.updated_at_ms)
                {
                    // Local rate edit is newer; push wins.
                } else {
                    match encrypt::open_json_b64::<serde_json::Value>(
                        pi_key,
                        &aad::rate_config(user_id, pi_id),
                        ct,
                    ) {
                        Ok(doc) => {
                            access.store_doc(&doc, &store, rcfg.updated_at_ms).context("sync pull: save rate config")?;
                        }
                        Err(e) => return Err(e).context("sync pull: open rate config"),
                    }
                }
            }
        }


    }

    Ok(())
}

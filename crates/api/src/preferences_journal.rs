//! Recover local rate saves across the SQLite/filesystem publication boundary.
//! Only rate fields and their fingerprints enter the drive database.
use std::path::Path;
use anyhow::{Context,Result,ensure};
use serde::{Deserialize,Serialize};
use serde_json::{Map,Value};
use sentryusb_drives::{DriveStore,mutable_intent::{RATE_KEYS,RATE_PREFERENCES_JOURNAL},schema};
use super::file_store;
use sentryusb_drives::rate_confirmation::Confirmation;

#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    version:u8,
    path:String,
    before_hash:String,
    after_hash:String,
    rates:Map<String,Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirmation:Option<Confirmation>,
}
fn hash(prefs:&Map<String,Value>)->Result<String> {
    // serde_json's map representation is ordered in this workspace. Sorting
    // explicitly also preserves the check if preserve_order is enabled later.
    fn canonical(value:Value)->Value {
        match value {
            Value::Object(map)=>Value::Object(map.into_iter().collect::<std::collections::BTreeMap<_,_>>()
                .into_iter().map(|(key,value)|(key,canonical(value))).collect()),
            Value::Array(values)=>Value::Array(values.into_iter().map(canonical).collect()),
            value=>value,
        }
    }
    Ok(hex::encode(ring::digest::digest(&ring::digest::SHA256,&serde_json::to_vec(&canonical(Value::Object(prefs.clone())))?)))
}
fn rates(prefs:&Map<String,Value>)->Map<String,Value> {
    RATE_KEYS.iter().filter_map(|key|prefs.get(*key).map(|value|((*key).to_string(),value.clone()))).collect()
}
fn apply_rates(prefs:&mut Map<String,Value>,rates:&Map<String,Value>) {
    for key in RATE_KEYS {
        match rates.get(*key) {Some(value)=>{prefs.insert((*key).into(),value.clone());},None=>{prefs.remove(*key);}}
    }
}
fn finish(store:&DriveStore,raw:&str)->Result<()> {
    store.with_locked_conn(|conn| {
        let tx=conn.unchecked_transaction()?;
        ensure!(schema::meta_get(&tx,RATE_PREFERENCES_JOURNAL)?.as_deref()==Some(raw),"rate preference journal changed");
        schema::meta_del(&tx,RATE_PREFERENCES_JOURNAL)?;tx.commit()?;Ok(())
    })
}

/// The caller holds PREFS_LOCK. Recover before any new edit or rate upload read.
pub(super) fn recover(primary:&Path,legacy:&Path,store:&DriveStore)->Result<Map<String,Value>> {
    let raw=store.with_locked_conn(|conn|schema::meta_get(conn,RATE_PREFERENCES_JOURNAL))?;
    let mut current=file_store::load(primary,legacy)?;
    let Some(raw)=raw else {return Ok(current)};
    let pending:Pending=serde_json::from_str(&raw).context("unreadable pending rate preferences")?;
    ensure!(((pending.version==1 && pending.confirmation.is_none()) || (pending.version==2 && pending.confirmation.is_some())) && pending.path==primary.to_str().context("invalid preferences path")?
        && pending.rates.keys().all(|key|RATE_KEYS.contains(&key.as_str())),"pending rate preference source changed");
    if let Some(confirmation) = &pending.confirmation { store.check_rate_confirmation(confirmation, &raw)?; }
    let current_hash=hash(&rates(&current))?;
    if current_hash==pending.before_hash {
        // Only the queued rate fields are recovered. Unrelated preferences
        // may have changed since the interruption and remain untouched.
        apply_rates(&mut current,&pending.rates);
    } else {
        ensure!(current_hash==pending.after_hash,"rates changed after an interrupted save; reconciliation required");
    }
    ensure!(hash(&pending.rates)?==pending.after_hash && rates(&current)==pending.rates,
        "pending rate values do not match their fingerprint");
    // Repeat the directory flush before retiring an uncertain rename outcome.
    file_store::save(primary,&current)?;
    crate::charging::invalidate_charging_list();
    match &pending.confirmation {
        Some(confirmation) => store.finish_rate_confirmation(confirmation, &raw)?,
        None => finish(store,&raw)?,
    }
    Ok(current)
}

/// Prepare SQLite intent first, publish the complete requested file second,
/// and retire the exact journal last. File failures leave recoverable intent.
pub(super) fn publish(primary:&Path,store:&DriveStore,before:&Map<String,Value>,after:&Map<String,Value>,force:bool)->Result<()> {
    publish_with_tag(primary,store,before,after,force,None)
}
pub(super) fn publish_with_tag(primary:&Path,store:&DriveStore,before:&Map<String,Value>,after:&Map<String,Value>,
    force:bool,required_tag:Option<&str>)->Result<()> {
    let after_rates=rates(after);
    let pending=Pending {version:1,path:primary.to_str().context("invalid preferences path")?.into(),
        before_hash:hash(&rates(before))?,after_hash:hash(&after_rates)?,rates:after_rates.clone(),confirmation:None};
    let raw=serde_json::to_string(&pending)?;
    store.stage_rate_preferences_with_tag(&Value::Object(rates(before)),&Value::Object(after_rates),force,required_tag,&raw)?;
    file_store::save(primary,after)?;
    crate::charging::invalidate_charging_list();
    finish(store,&raw)
}

/// Caller holds the preferences lock and current pairing guard. Replay only
/// newer edits over the current confirmed Cloud document, without creating an
/// outgoing echo. Recovery completes the same file/receipt transaction later.
pub(super) fn confirm(primary:&Path, legacy:&Path, store:&DriveStore, cloud:&Value,
    through:i64, receipt:Option<(&str,&str)>) -> Result<()> {
    let before = recover(primary, legacy, store)?;
    let snapshot = store.rate_sync_snapshot()?;
    let merged = sentryusb_drives::rate_intent::replay_after(&snapshot.intent, through, cloud)?;
    let desired = rates(merged.as_object().context("invalid confirmed rate document")?);
    let mut after = before.clone();
    apply_rates(&mut after, &desired);
    let confirmation = Confirmation { through, snapshot, receipt:receipt.map(|(key,raw)|(key.into(),raw.into())) };
    let pending = Pending { version:2, path:primary.to_str().context("invalid preferences path")?.into(),
        before_hash:hash(&rates(&before))?, after_hash:hash(&desired)?, rates:desired, confirmation:Some(confirmation) };
    let raw = serde_json::to_string(&pending)?;
    let confirmation = pending.confirmation.as_ref().unwrap();
    store.stage_rate_confirmation(confirmation, &raw)?;
    file_store::save(primary, &after)?;
    crate::charging::invalidate_charging_list();
    store.finish_rate_confirmation(confirmation, &raw)
}

#[cfg(test)]
#[path="preferences_journal/tests.rs"]
mod tests;

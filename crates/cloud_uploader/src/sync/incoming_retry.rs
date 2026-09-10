//! Durable refresh requests for individually unreadable incoming records.
//! Store identities, never stale plaintext or ciphertext to replay later.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sentryusb_drives::schema;
use super::revision::Kind;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Target {pub(super) kind:Kind,pub(super) id:String}
#[derive(Serialize, Deserialize)]
struct Retry {version:u8,binding:String,target:Target,nonce:String}
pub(super) struct Entry {pub(super) key:String,pub(super) raw:String,pub(super) target:Target}
pub(super) fn prefix(binding:&str)->String {format!("cloud_incoming_retry_v2:{binding}:")}
fn cursor_key(binding:&str)->String {format!("cloud_incoming_retry_cursor_v2:{binding}")}
fn key(binding:&str,target:&Target)->String {format!("{}{}:{}",prefix(binding),target.kind.name(),target.id)}

pub(super) fn generation(conn:&Connection,binding:&str)->Result<i64> {
    Ok(schema::meta_get(conn,&format!("cloud_incoming_retry_generation_v2:{binding}"))?
        .map(|raw|raw.parse::<i64>()).transpose()?.unwrap_or(0))
}
fn bump_generation(conn:&Connection,binding:&str)->Result<()> {
    let next=generation(conn,binding)?.checked_add(1).context("incoming retry generation exhausted")?;
    schema::meta_set(conn,&format!("cloud_incoming_retry_generation_v2:{binding}"),&next.to_string())
}

/// Call inside the same transaction that publishes the feed cursor/checkpoint.
pub(super) fn record(conn:&Connection,binding:&str,failed:&[Target],done:&[Target])->Result<()> {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce)
        .map_err(|_|anyhow::anyhow!("incoming retry nonce unavailable"))?;
    let nonce:String=nonce.iter().map(|byte|format!("{byte:02x}")).collect();
    for target in failed {
        let raw=serde_json::to_string(&Retry {version:1,binding:binding.into(),target:target.clone(),nonce:nonce.clone()})?;
        schema::meta_set(conn,&key(binding,target),&raw)?;
    }
    for target in done {schema::meta_del(conn,&key(binding,target))?;}
    if !failed.is_empty() || !done.is_empty() {bump_generation(conn,binding)?;}
    Ok(())
}

pub(super) fn entries(conn:&Connection,binding:&str)->Result<Vec<Entry>> {
    let prefix=prefix(binding);let end=format!("{prefix}~");
    let read=|after:&str|->Result<Vec<(String,String)>> {
        let mut query=conn.prepare_cached("SELECT key,value FROM meta WHERE key>=?1 AND key<?2 AND key>?3 ORDER BY key LIMIT 100")?;
        let rows=query.query_map(params![prefix,end,after],|row|Ok((row.get(0)?,row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    };
    let cursor=schema::meta_get(conn,&cursor_key(binding))?.unwrap_or_default();
    let rows=read(&cursor)?;let rows=if rows.is_empty() {read("")?} else {rows};
    rows.into_iter().map(|(saved_key,raw)| {
        let parsed:Retry=serde_json::from_str(&raw).context("unreadable incoming retry")?;
        ensure!(parsed.version==1 && parsed.binding==binding && key(binding,&parsed.target)==saved_key,
            "incoming retry identity mismatch");
        ensure!(match parsed.target.kind {
            Kind::RateConfig=>!parsed.target.id.is_empty() && parsed.target.id.len()<=64,
            _=>super::route_pull::hex_id(&parsed.target.id),
        },"invalid incoming retry target");
        Ok(Entry {key:saved_key,raw,target:parsed.target})
    }).collect()
}
pub(super) fn current(conn:&Connection,entry:&Entry)->Result<bool> {
    Ok(schema::meta_get(conn,&entry.key)?.as_deref()==Some(entry.raw.as_str()))
}
pub(super) fn complete(conn:&Connection,binding:&str,entry:&Entry)->Result<()> {
    if current(conn,entry)? {schema::meta_del(conn,&entry.key)?;bump_generation(conn,binding)?;}
    Ok(())
}
pub(super) fn advance(conn:&Connection,binding:&str,last:&Entry)->Result<()> {
    schema::meta_set(conn,&cursor_key(binding),&last.key)
}
pub(super) fn count(conn:&Connection,binding:&str)->Result<i64> {
    let start=prefix(binding);
    Ok(conn.query_row("SELECT COUNT(*) FROM meta WHERE key>=?1 AND key<?2",params![start,format!("{start}~")],|row|row.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentryusb_drives::DriveStore;
    fn target(id:u32)->Target {Target {kind:Kind::Charge,id:format!("{id:064x}")}}
    #[test]
    fn cursor_and_failed_record_storage_commit_or_roll_back_together() {
        let store=DriveStore::open_memory().unwrap();
        store.with_locked_conn(|conn| -> Result<()> {
            schema::meta_set(conn,"synthetic-feed-cursor","before")?;
            let tx=conn.unchecked_transaction()?;
            record(&tx,"pairing",&[target(1)],&[])?;
            schema::meta_set(&tx,"synthetic-feed-cursor","after")?;
            tx.rollback()?;
            assert_eq!(count(conn,"pairing")?,0);
            assert_eq!(schema::meta_get(conn,"synthetic-feed-cursor")?.as_deref(),Some("before"));
            let tx=conn.unchecked_transaction()?;record(&tx,"pairing",&[target(1)],&[])?;
            schema::meta_set(&tx,"synthetic-feed-cursor","after")?;tx.commit()?;
            assert_eq!(count(conn,"pairing")?,1);Ok(())
        }).unwrap();
    }
    #[test]
    fn replaced_retry_survives_a_late_completion_and_pairings_are_isolated() {
        let store=DriveStore::open_memory().unwrap();
        store.with_locked_conn(|conn| -> Result<()> {
            record(conn,"first",&[target(1)],&[])?;let old=entries(conn,"first")?.remove(0);
            record(conn,"first",&[target(1)],&[])?;record(conn,"second",&[target(1)],&[])?;
            assert!(!current(conn,&old)?);complete(conn,"first",&old)?;
            assert_eq!(count(conn,"first")?,1);assert_eq!(count(conn,"second")?,1);
            record(conn,"first",&[],&[target(1)])?;
            assert_eq!(count(conn,"first")?,0);assert_eq!(count(conn,"second")?,1);Ok(())
        }).unwrap();
    }
    #[test]
    fn permanently_failing_first_batch_cannot_starve_later_records() {
        let store=DriveStore::open_memory().unwrap();
        store.with_locked_conn(|conn| -> Result<()> {
            record(conn,"pairing",&(0..205).map(target).collect::<Vec<_>>(),&[])?;
            let first=entries(conn,"pairing")?;assert_eq!(first.len(),100);advance(conn,"pairing",first.last().unwrap())?;
            let second=entries(conn,"pairing")?;assert_eq!(second.len(),100);assert_eq!(second[0].target.id,target(100).id);
            advance(conn,"pairing",second.last().unwrap())?;
            let third=entries(conn,"pairing")?;assert_eq!(third.len(),5);advance(conn,"pairing",third.last().unwrap())?;
            assert_eq!(entries(conn,"pairing")?[0].target.id,target(0).id);Ok(())
        }).unwrap();
    }
}

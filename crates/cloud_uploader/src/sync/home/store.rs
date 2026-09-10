use super::*;
use sentryusb_drives::{DriveStore,schema};
use rusqlite::params;
fn walk_key(binding:&str)->String {format!("cloud_home_walk_v1:{binding}")}
pub(super) fn attempt_key(binding:&str,id:&str)->String {format!("cloud_home_attempt_v1:{binding}:{id}")}
fn retry_prefix(binding:&str)->String {format!("cloud_home_retry_v1:{binding}:")}
fn retry_key(binding:&str,id:&str)->String {format!("{}{id}",retry_prefix(binding))}
fn retry_cursor(binding:&str)->String {format!("cloud_home_retry_cursor_v1:{binding}")}
pub(super) fn load(store:&DriveStore,binding:&str)->Result<Option<String>> {store.with_locked_conn(|conn|schema::meta_get(conn,&walk_key(binding)))}
pub(super) fn attempt(store:&DriveStore,binding:&str,id:&str)->Result<Option<String>> {store.with_locked_conn(|conn|schema::meta_get(conn,&attempt_key(binding,id)))}
pub(super) fn reserve(store:&DriveStore,binding:&str,items:&[Ready])->Result<()> {
    store.with_durable_conn(|conn| {
        let tx=conn.transaction()?;
        for item in items {
            let key=attempt_key(binding,&item.id);
            ensure!(schema::meta_get(&tx,&key)?.as_deref()==item.expected.as_deref(),"Home attempt changed");
            schema::meta_set(&tx,&key,&item.raw)?;
            // This identity survives a stop before page acknowledgement even
            // when a successful write moved the source beyond the page fence.
            schema::meta_set(&tx,&retry_key(binding,&item.id),&item.id)?;
        }
        tx.commit()?;Ok(())
    })
}
pub(super) struct Outcome {pub id:String,pub done:bool,pub retire:Option<String>}
pub(super) fn finish(store:&DriveStore,binding:&str,outcomes:&[Outcome],walk:Option<(Option<&str>,&Walk)>,retry_after:Option<&str>)->Result<()> {
    store.with_durable_conn(|conn| {
        let tx=conn.transaction()?;
        if let Some((expected,_))=walk {ensure!(schema::meta_get(&tx,&walk_key(binding))?.as_deref()==expected,"Home progress changed");}
        for outcome in outcomes {
            if let Some(raw)=&outcome.retire {
                let key=attempt_key(binding,&outcome.id);
                ensure!(schema::meta_get(&tx,&key)?.as_deref()==Some(raw),"Home confirmation changed");schema::meta_del(&tx,&key)?;
            }
            if outcome.done {schema::meta_del(&tx,&retry_key(binding,&outcome.id))?;}
            else {schema::meta_set(&tx,&retry_key(binding,&outcome.id),&outcome.id)?;}
        }
        if let Some((_,walk))=walk {schema::meta_set(&tx,&walk_key(binding),&serde_json::to_string(walk)?)?;}
        if let Some(after)=retry_after {schema::meta_set(&tx,&retry_cursor(binding),after)?;}
        tx.commit()?;Ok(())
    })
}
pub(super) fn retries(store:&DriveStore,binding:&str)->Result<Vec<String>> {
    store.with_locked_conn(|conn| {
        let prefix=retry_prefix(binding);let end=format!("{prefix}~");
        let read=|after:&str|->Result<Vec<String>> {
            let mut query=conn.prepare_cached("SELECT key,value FROM meta WHERE key>=?1 AND key<?2 AND key>?3 ORDER BY key LIMIT 100")?;
            let rows=query.query_map(params![prefix,end,after],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(|(key,id)| {ensure!(wire::hex(&id) && key==retry_key(binding,&id),"Home retry source changed");Ok(id)}).collect()
        };
        let after=schema::meta_get(conn,&retry_cursor(binding))?.unwrap_or_default();let rows=read(&retry_key(binding,&after))?;let rows=if rows.is_empty(){read("")?}else{rows};
        ensure!(rows.iter().all(|id|wire::hex(id)),"invalid Home retry identity");Ok(rows)
    })
}
pub(super) fn remaining(store:&DriveStore,binding:&str)->Result<bool> {
    store.with_locked_conn(|conn| {let prefix=retry_prefix(binding);Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key>=?1 AND key<?2)",params![prefix,format!("{prefix}~")],|row|row.get(0))?)})
}

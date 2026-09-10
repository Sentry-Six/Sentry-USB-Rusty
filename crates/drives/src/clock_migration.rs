//! Install the clock transition inside the caller's SQLite transaction.
//! Originals and scoped recovery data are retained before display keys change.
use super::*;
use std::collections::{BTreeMap,BTreeSet};
use crate::{drive_edit_scope::Scope,grouper::DriveClipWindow};

pub(super) const VERSION_KEY:&str="drive_frame_clock_version";

fn tags(conn:&Connection)->Result<BTreeMap<String,Vec<String>>> {
    let mut statement=conn.prepare_cached("SELECT drive_key,tag FROM drive_tags ORDER BY drive_key,tag")?;
    let mut result=BTreeMap::<String,Vec<String>>::new();
    for row in statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))? {
        let (key,tag)=row?;result.entry(key).or_default().push(tag);
    }
    Ok(result)
}

// Only the source-window data is copied from an existing receipt. Its complete
// raw value, encrypted proposals, operation IDs and member outcomes stay intact.
fn receipt_scope(conn:&Connection,summaries:&[RouteSummary],drive:&str,through:i64)->Result<Option<Scope>> {
    let mut query=conn.prepare_cached("SELECT key,value FROM meta WHERE key LIKE 'cloud_drive_publication_v2:%' AND substr(key,-length(?1)-1)=':'||?1")?;
    let mut selected:Option<Scope>=None;
    for row in query.query_map([drive],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))? {
        let (key,raw)=row?;
        let value:serde_json::Value=serde_json::from_str(&raw).context("unreadable drive publication during clock migration")?;
        anyhow::ensure!(value["drive"].as_str()==Some(drive),"drive receipt key and selection disagree");
        let binding=value["binding"].as_str().context("drive receipt has no binding")?;
        anyhow::ensure!(value["version"]==1 && key==format!("cloud_drive_publication_v2:{binding}:{drive}"),
            "unsupported drive publication during clock migration");
        // Without edit-time scope evidence a newer legacy edit cannot be
        // assumed to have selected the older publication's exact membership.
        anyhow::ensure!(value["through"].as_i64()==Some(through),"newer legacy drive edit requires scope reconciliation");
        let drives=value["local"]["drives"].as_array().context("drive receipt has no windows")?;
        anyhow::ensure!(drives.len()==1 && drives[0][0].as_str()==Some(drive),"drive receipt selection changed");
        let windows:Vec<DriveClipWindow>=serde_json::from_value(drives[0][1].clone())?;
        let expected=Scope {version:1,windows:windows.clone(),sources:serde_json::from_value(value["local"]["sources"].clone())?};
        let fresh=capture_scope_windows(conn,summaries,windows)?;
        let mut comparable=expected.clone();
        // A receipt may precede installation of the authenticated wrapped key
        // into the local cache. The saved receipt still pins that key for replay.
        for (file,source) in &mut comparable.sources {
            if fresh.sources.get(file).is_some_and(|source|source.key.is_none()) {source.key=None;}
        }
        anyhow::ensure!(comparable.matches(&fresh),"pending drive original changed before clock migration");
        if let Some(previous)=&selected {anyhow::ensure!(previous==&fresh,"ambiguous legacy drive receipt scopes");}
        selected=Some(fresh);
    }
    Ok(selected)
}

pub(super) fn install(conn:&Connection, explicit_import_tags:Option<&std::collections::HashMap<String,Vec<String>>>)->Result<bool> {
    anyhow::ensure!(!conn.is_autocommit(),"clock migration requires a transaction");
    if schema::meta_get(conn,VERSION_KEY)?.as_deref()==Some("2") {return Ok(false)}
    let summaries=select_all_route_summaries(conn)?;
    let original_tags=tags(conn)?;
    let timing=crate::grouper::timing_compatibility::plan(&summaries);
    let mut legacy_windows=BTreeMap::<String,Vec<DriveClipWindow>>::new();
    for drive in timing.legacy {
        legacy_windows.entry(drive.start_time).or_default().extend(drive.windows);
    }
    let current_keys:BTreeSet<_>=timing.corrected.iter().map(|drive|drive.start_time.as_str()).collect();
    let queued=conn.prepare_cached("SELECT key,changed_at FROM mutable_dirty WHERE kind='drive' ORDER BY key")?
        .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut overrides=BTreeMap::new();let mut frozen=BTreeMap::new();
    for (key,through) in queued {
        // A replacement import keeps pre-import outboxes bound to their old
        // sources; it must not project those edits onto replacement recordings.
        if explicit_import_tags.is_some() {continue}
        if current_keys.contains(key.as_str()) {continue}
        let saved=read_drive_edit_scope(conn,&key)?;
        let scope=match saved {
            Some(scope)=>scope,
            None=>match receipt_scope(conn,&summaries,&key,through)? {
                Some(scope)=>Some(scope),
                None=>legacy_windows.get(&key).map(|windows|capture_scope_windows(conn,&summaries,windows.clone())).transpose()?,
            },
        };
        if let Some(scope)=&scope {
            let fresh=capture_scope_windows(conn,&summaries,scope.windows.clone())?;
            anyhow::ensure!(scope.matches(&fresh),"saved drive edit source changed before clock migration");
            overrides.insert(key.clone(),scope.windows.clone());
        }
        conn.execute("INSERT OR IGNORE INTO mutable_drive_scope(kind,key,payload) VALUES('drive',?1,?2)",
            params![key,serde_json::to_string(&scope)?])?;
        frozen.insert(key,scope);
    }
    let mut applicable_tags=original_tags.clone();
    for (key,scope) in &frozen {
        if scope.is_none() {applicable_tags.remove(key);}
    }
    let mut plan=crate::drive_tag_migration::plan_with_scopes(&summaries,&applicable_tags,&overrides)?;
    plan.original_tags=original_tags.clone();
    plan.unresolved_keys.extend(frozen.iter().filter(|(_,scope)|scope.is_none()).map(|(key,_)|key.clone()));
    plan.unresolved_keys.sort();plan.unresolved_keys.dedup();
    let source_revision:i64=conn.query_row("SELECT revision FROM mutable_route_source_clock WHERE id=1",[],|row|row.get(0))?;
    let snapshot=serde_json::json!({"version":1,"sourceRevision":source_revision,"tags":plan,
        "queuedScopes":frozen,"explicitImportTags":explicit_import_tags});
    conn.execute("INSERT INTO drive_clock_migrations(snapshot) VALUES(?1)",[serde_json::to_string(&snapshot)?])?;
    let migration_id=conn.last_insert_rowid();
    let mut converted:BTreeSet<String>=plan.legacy.iter().map(|drive|drive.key.clone()).collect();
    converted.extend(frozen.keys().cloned());
    for key in &converted {
        conn.execute("INSERT INTO drive_clock_legacy_keys(key,migration_id) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET migration_id=excluded.migration_id",
            params![key,migration_id])?;
    }
    for (key,labels) in &plan.corrected_tags {
        // A current-format imported view, including an explicit empty one,
        // supersedes an older-key projection for that same current drive.
        let labels=explicit_import_tags.and_then(|tags|tags.get(key)).or_else(||original_tags.get(key)).unwrap_or(labels);
        conn.execute("DELETE FROM drive_tags WHERE drive_key=?1",[key])?;
        for tag in labels {
            conn.execute("INSERT OR IGNORE INTO drive_tags(drive_key,tag) VALUES(?1,?2)",params![key,tag])?;
        }
    }
    // Retain the legacy rows as well as the scoped snapshot. They are excluded
    // from current exports and suggestions, and remain readable for recovery.
    schema::meta_set(conn,VERSION_KEY,"2")?;
    Ok(true)
}

/// Merge-import keeps the existing current labels and adds labels from the
/// imported legacy frame scopes. It never republishes those display unions.
pub(super) fn merge_import(conn:&Connection,incoming:&std::collections::HashMap<String,Vec<String>>,
    files:&std::collections::HashSet<String>)->Result<()> {
    anyhow::ensure!(!conn.is_autocommit(),"clock tag import requires a transaction");
    if schema::meta_get(conn,VERSION_KEY)?.as_deref()!=Some("2") || incoming.is_empty() {return Ok(())}
    let summaries=select_all_route_summaries(conn)?;
    let selected:Vec<_>=summaries.iter().filter(|summary|files.is_empty() || files.contains(&summary.file)).cloned().collect();
    let imported=incoming.iter().map(|(key,labels)|(key.clone(),labels.clone())).collect();
    let plan=crate::drive_tag_migration::plan(&selected,&imported)?;
    if plan.legacy.is_empty() {return Ok(())}
    let current=tags(conn)?;
    let snapshot=serde_json::json!({"version":1,"mergeImport":true,"tags":plan});
    conn.execute("INSERT INTO drive_clock_migrations(snapshot) VALUES(?1)",[serde_json::to_string(&snapshot)?])?;
    let migration_id=conn.last_insert_rowid();
    for (key,windows) in crate::grouper::drive_key_clip_windows(&summaries) {
        let mut labels:BTreeSet<String>=current.get(&key).into_iter().flatten().cloned().collect();
        let mut touched=false;
        for window in windows {
            if let Some(source)=plan.sources.get(&window.file) {
                anyhow::ensure!(source.total_frames==window.total_frames,"imported tag source frame count changed");
                labels.extend(crate::scoped_tags::tags_for_ranges(&source.document,source.total_frames,
                    &[(window.start_frame,window.end_frame)])?);
                touched=true;
            }
        }
        if touched {
            for label in labels {
                conn.execute("INSERT OR IGNORE INTO drive_tags(drive_key,tag) VALUES(?1,?2)",params![key,label])?;
            }
        }
    }
    for old in &plan.legacy {
        conn.execute("INSERT INTO drive_clock_legacy_keys(key,migration_id) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET migration_id=excluded.migration_id",
            params![old.key,migration_id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy_store()->DriveStore {
        let store=DriveStore::open_memory().unwrap();
        for (file,runs) in [("2026-01-01_12-00-00-front.mp4",vec![(4,20),(0,5),(4,35)]),
            ("2026-01-01_12-00-07-front.mp4",vec![(0,60)])] {
            let gears:Vec<_>=runs.iter().flat_map(|&(gear,frames)|vec![gear;frames]).collect();
            let n=gears.len();let points:Vec<_>=(0..n).map(|i|[37.0+i as f64*0.00001,-122.0]).collect();
            let gear_runs=runs.iter().map(|&(gear,frames)|GearRun {gear,frames:frames as u32}).collect::<Vec<_>>();
            store.add_route(file,"2026-01-01",&points,&gears,&vec![0;n],&vec![2.0;n],&vec![0.0;n],
                gears.iter().filter(|&&gear|gear==0).count() as u32,n as u32,&gear_runs,&[]).unwrap();
        }
        store.set_drive_tags_from_sync("2026-01-01T12:00:00",&["Work".into()]).unwrap();
        store.set_drive_tags_from_sync("2026-01-01T12:00:25",&["Personal".into()]).unwrap();
        store
    }
    fn migrate(store:&DriveStore)->Result<bool> {
        store.with_durable_conn(|conn| {let tx=conn.transaction()?;let changed=install(&tx,None)?;tx.commit()?;Ok(changed)})
    }
    #[test]
    fn migration_keeps_scoped_recovery_and_is_idempotent() {
        let store=legacy_store();let revision=store.mutable_route_source_revision().unwrap();
        assert!(migrate(&store).unwrap());
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:00.000").unwrap(),vec!["Personal","Work"]);
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:25").unwrap(),vec!["Personal"]);
        let raw=store.with_locked_conn(|conn|conn.query_row("SELECT snapshot FROM drive_clock_migrations",[],|row|row.get::<_,String>(0))).unwrap();
        let archive:serde_json::Value=serde_json::from_str(&raw).unwrap();
        assert_eq!(archive["tags"]["original_tags"]["2026-01-01T12:00:25"],serde_json::json!(["Personal"]));
        assert_eq!(archive["tags"]["legacy"].as_array().unwrap().len(),2);
        assert_eq!(store.mutable_route_source_revision().unwrap(),revision);
        assert!(!migrate(&store).unwrap());
        assert_eq!(store.with_locked_conn(|conn|conn.query_row("SELECT COUNT(*) FROM drive_clock_migrations",[],|row|row.get::<_,i64>(0))).unwrap(),1);
    }
    #[test]
    fn startup_migration_survives_an_independent_wal_writer() {
        use std::sync::{Arc,atomic::{AtomicBool,Ordering},mpsc};
        let directory=tempfile::tempdir().unwrap();
        let path=directory.path().join("concurrent.db");
        let seed=legacy_store();
        seed.with_locked_conn(|conn| {
            // Enough distinct source groups to exercise the read/plan interval.
            let columns=conn.prepare("PRAGMA table_info(routes)")?
                .query_map([],|row|row.get::<_,String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let names=columns.iter().map(|name|format!("\"{name}\"")).collect::<Vec<_>>().join(",");
            let values=columns.iter().map(|name|if name=="file" {
                "strftime('%Y-%m-%d_%H-%M-%S','2026-02-01','+'||n||' minutes')||'-front.mp4'".to_string()
            } else {format!("r.\"{name}\"")}).collect::<Vec<_>>().join(",");
            conn.execute(&format!("WITH RECURSIVE sequence(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM sequence WHERE n<500) INSERT INTO routes({names}) SELECT {values} FROM sequence CROSS JOIN routes r WHERE r.file='2026-01-01_12-00-00-front.mp4'"),[])?;
            schema::meta_set(conn,"imported_from_json_at","synthetic")?;
            conn.execute("VACUUM INTO ?1",[path.to_str().unwrap()])?;
            Ok::<_,anyhow::Error>(())
        }).unwrap();
        let writer_path=path.clone();let stop=Arc::new(AtomicBool::new(false));let writer_stop=stop.clone();
        let (ready_tx,ready_rx)=mpsc::channel();
        let writer=std::thread::spawn(move || {
            let conn=Connection::open(writer_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;").unwrap();
            let mut writes=0;
            while !writer_stop.load(Ordering::Acquire) {
                conn.execute("INSERT INTO meta(key,value) VALUES('synthetic_sampler_tick',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[writes.to_string()]).unwrap();
                writes+=1;if writes==1 {ready_tx.send(()).unwrap();}
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            writes
        });
        ready_rx.recv().unwrap();
        let migrated=DriveStore::open(path.to_str().unwrap());
        stop.store(true,Ordering::Release);assert!(writer.join().unwrap()>1);
        let migrated=migrated.unwrap();
        assert_eq!(migrated.with_read_conn(|conn|schema::meta_get(conn,VERSION_KEY)).unwrap().as_deref(),Some("2"));
        assert_eq!(migrated.get_drive_tags("2026-01-01T12:00:00.000").unwrap(),vec!["Personal","Work"]);
    }
    #[test]
    fn failed_projection_rolls_back_archive_old_tags_and_version_together() {
        let store=legacy_store();
        store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_clock BEFORE INSERT ON drive_tags WHEN NEW.drive_key LIKE '%.000' BEGIN SELECT RAISE(ABORT,'synthetic clock failure'); END;")).unwrap();
        assert!(migrate(&store).is_err());
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:00").unwrap(),vec!["Work"]);
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:25").unwrap(),vec!["Personal"]);
        assert!(store.with_locked_conn(|conn|schema::meta_get(conn,VERSION_KEY)).unwrap().is_none());
        assert_eq!(store.with_locked_conn(|conn|conn.query_row("SELECT COUNT(*) FROM drive_clock_migrations",[],|row|row.get::<_,i64>(0))).unwrap(),0);
    }
    #[test]
    fn old_queue_is_frozen_before_keys_move_without_rewriting_intent() {
        let store=legacy_store();let key="2026-01-01T12:00:25";
        store.with_locked_conn(|conn| {
            let tx=conn.unchecked_transaction()?;
            mark_mutable_dirty_at(&tx,"drive",key,1)?;
            mutable_intent::record(&tx,"drive",key,None,&mutable_intent::tag_edit(&[],&["Personal".into()]))?;
            tx.commit()?;Ok::<_,anyhow::Error>(())
        }).unwrap();
        let before=store.with_locked_conn(|conn|mutable_intent::read(conn,"drive",key)).unwrap();
        migrate(&store).unwrap();
        let scope=store.drive_edit_scope(key).unwrap().unwrap().unwrap();
        assert_eq!((scope.windows[0].start_frame,scope.windows[0].end_frame),(25,60));
        assert_eq!(store.get_drive_tags(key).unwrap(),vec!["Personal"]);
        assert_eq!(store.with_locked_conn(|conn|mutable_intent::read(conn,"drive",key)).unwrap(),before);
        assert_eq!(store.dirty_mutables().unwrap().len(),1);
        assert!(!store.get_data().unwrap().drive_tags.contains_key(key));
    }

    #[test]
    fn captured_unresolved_key_does_not_attach_to_a_later_matching_clip() {
        let store=legacy_store();let key="2026-01-01T12:00:25";
        store.with_locked_conn(|conn| {
            let tx=conn.unchecked_transaction()?;mark_mutable_dirty_at(&tx,"drive",key,1)?;
            tx.execute("INSERT INTO mutable_drive_scope(kind,key,payload) VALUES('drive',?1,'null')",[key])?;
            tx.commit()?;Ok::<_,anyhow::Error>(())
        }).unwrap();
        migrate(&store).unwrap();
        assert_eq!(store.drive_edit_scope(key).unwrap(),Some(None));
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:00.000").unwrap(),vec!["Work"]);
        assert!(!store.get_data().unwrap().drive_tags.contains_key(key));
    }

    #[test]
    fn pending_receipt_is_unchanged_and_its_original_windows_are_retained() {
        let store=legacy_store();let key="2026-01-01T12:00:25";
        let raw=store.with_locked_conn(|conn| {
            let tx=conn.unchecked_transaction()?;mark_mutable_dirty_at(&tx,"drive",key,1)?;
            let through=mutable_intent::queued(&tx,"drive",key)?.unwrap();
            let summaries=select_all_route_summaries(&tx)?;
            let old=crate::grouper::timing_compatibility::plan(&summaries).legacy.into_iter().find(|drive|drive.start_time==key).unwrap();
            let scope=capture_scope_windows(&tx,&summaries,old.windows)?;
            let raw=serde_json::json!({"version":1,"binding":"synthetic","drive":key,"through":through,
                "local":{"drives":[[key,scope.windows]],"sources":scope.sources},
                "opaqueProposal":{"unchanged":"synthetic receipt payload"}}).to_string();
            schema::meta_set(&tx,&format!("cloud_drive_publication_v2:synthetic:{key}"),&raw)?;
            tx.commit()?;Ok::<_,anyhow::Error>(raw)
        }).unwrap();
        migrate(&store).unwrap();
        assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,&format!("cloud_drive_publication_v2:synthetic:{key}"))).unwrap(),Some(raw));
        let scope=store.drive_edit_scope(key).unwrap().unwrap().unwrap();
        assert_eq!((scope.windows[0].start_frame,scope.windows[0].end_frame),(25,60));
    }

    #[test]
    fn current_format_empty_import_does_not_resurrect_legacy_tags() {
        let old=legacy_store();let mut data=old.get_data().unwrap();
        data.drive_tags.insert("2026-01-01T12:00:00.000".into(),vec![]);
        let restored=DriveStore::open_memory().unwrap();restored.replace_data(&data).unwrap();
        assert!(restored.get_drive_tags("2026-01-01T12:00:00.000").unwrap().is_empty());
        assert!(restored.get_data().unwrap().drive_tags.is_empty());
        restored.clear_all_drives().unwrap();
        assert_eq!(restored.with_locked_conn(|conn|conn.query_row("SELECT COUNT(*) FROM drive_clock_migrations",[],|row|row.get::<_,i64>(0))).unwrap(),0);
    }

    #[test]
    fn merge_import_translates_legacy_keys_and_preserves_current_labels() {
        let store=legacy_store();migrate(&store).unwrap();
        store.set_drive_tags_from_sync("2026-01-01T12:00:00.000",&["Current".into()]).unwrap();
        let path=tempfile::NamedTempFile::new().unwrap();
        let mut data=store.get_data().unwrap();
        data.drive_tags.clear();data.drive_tags.insert("2026-01-01T12:00:25".into(),vec!["Imported".into()]);
        std::fs::write(path.path(),serde_json::to_vec(&data).unwrap()).unwrap();
        store.import_json_file(path.path().to_str().unwrap()).unwrap();
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:00.000").unwrap(),vec!["Current","Imported"]);
        assert!(!store.get_data().unwrap().drive_tags.contains_key("2026-01-01T12:00:25"));
    }

    #[test]
    fn removing_migrated_tags_survives_both_export_paths_and_restore() {
        let store=legacy_store();migrate(&store).unwrap();
        store.set_drive_tags("2026-01-01T12:00:00.000",&[]).unwrap();
        assert!(store.get_all_tag_names().unwrap().is_empty());
        assert_eq!(store.get_drive_tags("2026-01-01T12:00:25").unwrap(),vec!["Personal"],"recovery row remains intact");
        let data=store.get_data().unwrap();assert!(data.drive_tags.is_empty());
        let mut output=Vec::new();store.with_locked_conn(|conn|crate::json_compat::export_json(conn,&mut output)).unwrap();
        let exported:StoreData=serde_json::from_slice(&output).unwrap();
        assert!(exported.drive_tags.is_empty());
        let restored=DriveStore::open_memory().unwrap();restored.replace_data(&data).unwrap();
        assert!(restored.get_all_tag_names().unwrap().is_empty());
    }

    #[test]
    fn startup_migrates_tags_before_building_the_current_drive_cache() {
        let store=legacy_store();store.load_locked(&[]).unwrap();
        let raw=store.with_locked_conn(|conn|schema::meta_get(conn,"drive_list_cache")).unwrap().unwrap();
        let cached:serde_json::Value=serde_json::from_str(&raw).unwrap();
        assert_eq!(cached.as_array().unwrap().len(),1);
        assert_eq!(cached[0]["startTime"],"2026-01-01T12:00:00.000");
        assert_eq!(cached[0]["tags"],serde_json::json!(["Personal","Work"]));
        assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,VERSION_KEY)).unwrap().as_deref(),Some("2"));
    }
}

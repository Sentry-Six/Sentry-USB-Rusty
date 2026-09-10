use super::*;

#[test]
fn unreadable_tags_are_errors_not_empty_sets() {
    let store = DriveStore::open_memory().unwrap();
    store.with_locked_conn(|conn| conn.execute_batch(
        "INSERT INTO drive_tags(drive_key, tag) VALUES('drive', X'FF');
         INSERT INTO charge_tags(session_ts, tag) VALUES(100, X'FF');"
    )).unwrap();
    assert!(store.get_drive_tags("drive").is_err());
    assert!(store.get_charge_tags(100).is_err());
}

#[test]
fn charge_apply_rechecks_edits_and_upload_identity_at_commit() {
    let store = DriveStore::open_memory().unwrap();
    store.charge_upload_mark(100, "charge", "key", 1).unwrap();
    store.set_charge_tags(100, &["Local".into()]).unwrap();
    let at = store.dirty_mutables().unwrap()[0].2;
    store.apply_charge_mutable_from_sync(100, "charge", "key", &["Cloud".into()], None, at).unwrap();
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Local"]);
    assert_eq!(store.dirty_mutables().unwrap().len(), 1);
    assert!(store.apply_charge_mutable_from_sync(100, "charge", "other-key", &[], None, at + 1).is_err());
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Local"]);
    // Even a newer whole-envelope timestamp cannot discard a pending field edit.
    store.apply_charge_mutable_from_sync(100, "charge", "key", &["Cloud".into()], Some((5.0, "CAD".into())), at + 1).unwrap();
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Local"]);
    assert_eq!(store.get_charge_cost(100).unwrap(), None);
    assert_eq!(store.dirty_mutables().unwrap().len(), 1);
    store.confirm_charge_mutable_push(100, at, "charge", "key", 1,
        &["Cloud".into(), "Local".into()], Some((5.0, "CAD".into()))).unwrap();
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Cloud", "Local"]);
    assert_eq!(store.get_charge_cost(100).unwrap(), Some((5.0, "CAD".into())));
    assert!(store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn deleted_upload_is_not_recreated_by_late_charge_apply() {
    let store = DriveStore::open_memory().unwrap();
    store.apply_charge_mutable_from_sync(100, "charge", "key", &["Cloud".into()], Some((5.0, "CAD".into())), 15).unwrap();
    assert!(store.get_charge_tags(100).unwrap().is_empty());
    assert_eq!(store.get_charge_cost(100).unwrap(), None);
}

#[test]
fn failed_cost_queue_write_rolls_back_the_cost_edit() {
    let store = DriveStore::open_memory().unwrap();
    store.set_charge_cost_from_sync(100, Some((4.0, "CAD".into()))).unwrap();
    store.with_locked_conn(|conn| conn.execute_batch(
        "CREATE TRIGGER fail_dirty BEFORE INSERT ON mutable_dirty BEGIN SELECT RAISE(ABORT, 'synthetic queue failure'); END;"
    )).unwrap();
    assert!(store.set_charge_cost(100, Some((8.0, "CAD".into()))).is_err());
    assert_eq!(store.get_charge_cost(100).unwrap(), Some((4.0, "CAD".into())));
    assert!(store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn queue_generations_survive_same_millisecond_clear_and_clock_reversal() {
    let store = DriveStore::open_memory().unwrap();
    let queue = |now| store.with_locked_conn(|conn| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        mark_mutable_dirty_at(&tx, "charge", "100", now)?;
        tx.commit()?;
        Ok(())
    }).unwrap();
    queue(100);
    assert_eq!(store.dirty_mutables().unwrap()[0].2, 100);
    store.clear_mutable_dirty("charge", "100", 100).unwrap();
    queue(100);
    assert_eq!(store.dirty_mutables().unwrap()[0].2, 101);
    store.clear_mutable_dirty("charge", "100", 100).unwrap();
    assert_eq!(store.dirty_mutables().unwrap().len(), 1);
    queue(50);
    assert_eq!(store.dirty_mutables().unwrap()[0].2, 102);
}

#[test]
fn mutable_snapshots_reject_old_generations_and_keep_cost_with_tags() {
    let store = DriveStore::open_memory().unwrap();
    store.charge_upload_mark(100, "charge", "key", 1).unwrap();
    store.set_charge_tags(100, &["Before".into()]).unwrap();
    let before = store.dirty_mutables().unwrap()[0].2;
    store.set_charge_cost(100, Some((8.0, "CAD".into()))).unwrap();
    let latest = store.dirty_mutables().unwrap()[0].2;
    assert!(latest > before);
    assert!(store.charge_mutable_for_sync(100, before).unwrap().is_none());
    let snapshot = store.charge_mutable_for_sync(100, latest).unwrap().unwrap();
    assert_eq!(snapshot.tags, vec!["Before"]);
    assert_eq!(snapshot.cost, Some((8.0, "CAD".into())));
    assert_eq!(snapshot.upload, Some(("charge".into(), "key".into(), 1)));
    store.set_drive_tags("drive", &["Work".into()]).unwrap();
    let at = store.dirty_mutables().unwrap().into_iter().find(|(kind, _, _)| kind == "drive").unwrap().2;
    assert_eq!(store.drive_tags_for_sync("drive", at).unwrap(), Some(vec!["Work".into()]));
    store.set_drive_tags("drive", &["Changed".into()]).unwrap();
    assert!(store.drive_tags_for_sync("drive", at).unwrap().is_none());
}

#[test]
fn intent_failure_rolls_back_value_queue_and_clock_together() {
    let store = DriveStore::open_memory().unwrap();
    store.set_charge_tags_from_sync(100, &["Before".into()]).unwrap();
    store.set_charge_cost_from_sync(100, Some((4.0, "CAD".into()))).unwrap();
    let before_clock = store.with_locked_conn(|conn| crate::schema::meta_get(conn, "cloud_mutable_local_clock_ms")).unwrap();
    store.with_locked_conn(|conn| conn.execute_batch(
        "CREATE TRIGGER fail_intent BEFORE INSERT ON mutable_intent_events BEGIN SELECT RAISE(ABORT, 'synthetic intent failure'); END;"
    )).unwrap();
    assert!(store.set_charge_tags(100, &["After".into()]).is_err());
    assert!(store.set_charge_cost(100, Some((8.0, "CAD".into()))).is_err());
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Before"]);
    assert_eq!(store.get_charge_cost(100).unwrap(), Some((4.0, "CAD".into())));
    assert!(store.dirty_mutables().unwrap().is_empty());
    assert_eq!(store.with_locked_conn(|conn| crate::schema::meta_get(conn, "cloud_mutable_local_clock_ms")).unwrap(), before_clock);
    assert_eq!(store.with_locked_conn(|conn| conn.query_row("SELECT COUNT(*) FROM mutable_intent_state", [], |row| row.get::<_,i64>(0))).unwrap(), 0);
}

#[test]
fn additive_schema_keeps_legacy_edits_explicit_until_their_prefix_is_confirmed() {
    let store = DriveStore::open_memory().unwrap();
    store.set_charge_tags(100, &["Before".into()]).unwrap();
    store.set_charge_cost_from_sync(100, Some((4.0, "CAD".into()))).unwrap();
    let queued = store.dirty_mutables().unwrap();
    store.with_locked_conn(|conn| conn.execute_batch("DROP TABLE mutable_intent_events; DROP TABLE mutable_intent_state;")).unwrap();
    store.with_locked_conn(crate::schema::migrate).unwrap();
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Before"]);
    assert_eq!(store.get_charge_cost(100).unwrap(), Some((4.0, "CAD".into())));
    assert_eq!(store.dirty_mutables().unwrap(), queued);
    let old = store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","100")).unwrap();
    assert!(old.legacy); assert!(old.edits.is_empty());
    store.set_charge_tags(100, &["Before".into(),"New".into()]).unwrap();
    let current = store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","100")).unwrap();
    assert!(current.legacy); assert_eq!(current.edits.len(),1);
    store.clear_mutable_dirty("charge","100",queued[0].2).unwrap();
    let remaining = store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","100")).unwrap();
    assert!(!remaining.legacy); assert_eq!(remaining.edits.len(),1);
    assert_eq!(store.get_charge_tags(100).unwrap(), vec!["Before","New"]);
}

#[test]
fn replacing_source_does_not_retire_events_and_completed_queue_cascades_them() {
    let store = DriveStore::open_memory().unwrap();
    store.charge_upload_mark(100,"charge","old-key",1).unwrap();
    store.set_charge_tags(100,&["New".into()]).unwrap();
    let at = store.dirty_mutables().unwrap()[0].2;
    store.charge_upload_mark(100,"charge","new-key",2).unwrap();
    store.clear_charge_mutable_dirty_if_source(100,at,"charge","old-key",1).unwrap();
    assert_eq!(store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","100")).unwrap().edits.len(),1);
    store.clear_charge_mutable_dirty_if_source(100,at,"charge","new-key",2).unwrap();
    assert!(store.dirty_mutables().unwrap().is_empty());
    assert_eq!(store.with_locked_conn(|conn| conn.query_row("SELECT COUNT(*) FROM mutable_intent_events",[],|row|row.get::<_,i64>(0))).unwrap(),0);
    assert_eq!(store.with_locked_conn(|conn| conn.query_row("SELECT COUNT(*) FROM mutable_intent_state",[],|row|row.get::<_,i64>(0))).unwrap(),0);
}

#[test]
fn merged_confirmation_adopts_cloud_cost_and_preserves_newer_local_tag_operations() {
    let store = DriveStore::open_memory().unwrap();
    store.charge_upload_mark(1,"charge","key",1).unwrap();
    store.set_charge_cost_from_sync(1,Some((4.0,"CAD".into()))).unwrap();
    store.set_charge_tags(1,&["First".into()]).unwrap();
    let through=store.dirty_mutables().unwrap()[0].2;
    store.set_charge_tags(1,&["Second".into()]).unwrap();
    assert!(store.confirm_charge_mutable_push(1,through,"charge","key",1,
        &["First".into(),"Cloud".into()],Some((6.0,"CAD".into()))).unwrap());
    assert_eq!(store.get_charge_tags(1).unwrap(),vec!["Cloud","Second"]);
    assert_eq!(store.get_charge_cost(1).unwrap(),Some((6.0,"CAD".into())));
    let remaining=store.with_locked_conn(|conn| mutable_intent::read(conn,"charge","1")).unwrap();
    assert_eq!(remaining.edits.len(),1);assert!(remaining.edits[0].0>through);
    assert_eq!(store.dirty_mutables().unwrap().len(),1);
}

#[test]
fn merged_confirmation_preserves_a_new_local_cost_and_checks_upload_source() {
    let store = DriveStore::open_memory().unwrap();
    store.charge_upload_mark(1,"charge","key",1).unwrap();
    store.set_charge_tags(1,&["Work".into()]).unwrap();let through=store.dirty_mutables().unwrap()[0].2;
    store.set_charge_cost(1,Some((8.0,"CAD".into()))).unwrap();
    assert!(!store.confirm_charge_mutable_push(1,through,"charge","wrong-key",1,&[],None).unwrap());
    assert_eq!(store.get_charge_tags(1).unwrap(),vec!["Work"]);
    assert!(store.confirm_charge_mutable_push(1,through,"charge","key",1,&["Cloud".into(),"Work".into()],Some((6.0,"CAD".into()))).unwrap());
    assert_eq!(store.get_charge_cost(1).unwrap(),Some((8.0,"CAD".into())));
    assert_eq!(store.get_charge_tags(1).unwrap(),vec!["Cloud","Work"]);
    let latest=store.dirty_mutables().unwrap()[0].2;
    assert!(store.confirm_charge_mutable_push(1,latest,"charge","key",1,&["Cloud".into(),"Work".into()],Some((8.0,"CAD".into()))).unwrap());
    assert!(store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn failed_confirmation_rolls_back_values_and_keeps_intent() {
    let store = DriveStore::open_memory().unwrap();store.charge_upload_mark(1,"charge","key",1).unwrap();
    store.set_charge_cost_from_sync(1,Some((4.0,"CAD".into()))).unwrap();
    store.set_charge_tags(1,&["Work".into()]).unwrap();let through=store.dirty_mutables().unwrap()[0].2;
    store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_confirm BEFORE INSERT ON charge_costs BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")).unwrap();
    assert!(store.confirm_charge_mutable_push(1,through,"charge","key",1,&["Cloud".into()],Some((6.0,"CAD".into()))).is_err());
    assert_eq!(store.get_charge_tags(1).unwrap(),vec!["Work"]);
    assert_eq!(store.get_charge_cost(1).unwrap(),Some((4.0,"CAD".into())));
    assert_eq!(store.with_locked_conn(|conn|mutable_intent::read(conn,"charge","1")).unwrap().edits.len(),1);
    assert_eq!(store.dirty_mutables().unwrap().len(),1);
}

#[test]
fn source_revision_blocks_late_drive_confirmation_after_ingestion() {
    let store=DriveStore::open_memory().unwrap();store.set_drive_tags("drive",&["Work".into()]).unwrap();
    let through=store.dirty_mutables().unwrap()[0].2;let source=store.mutable_route_source_revision().unwrap();
    store.add_route("2026-09-01_10-00-00-front.mp4","2026-09-01",&[[53.0,-113.0],[53.01,-113.01]],&[4,4],&[1,1],&[25.0,26.0],&[0.5,0.6],0,60,&[],&[]).unwrap();
    assert!(store.mutable_route_source_revision().unwrap()>source);
    assert!(!store.confirm_drive_mutable_push("drive",through,source,&["Cloud".into()]).unwrap());
    assert_eq!(store.get_drive_tags("drive").unwrap(),vec!["Work"]);assert_eq!(store.dirty_mutables().unwrap().len(),1);
    let fresh=store.mutable_route_source_revision().unwrap();
    assert!(store.confirm_drive_mutable_push("drive",through,fresh,&["Cloud".into(),"Work".into()]).unwrap());
    assert_eq!(store.get_drive_tags("drive").unwrap(),vec!["Cloud","Work"]);assert!(store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn route_source_revision_follows_commits_and_rolls_back_with_source_changes() {
    let store=DriveStore::open_memory().unwrap();
    store.add_route("2026-09-01_10-00-00-front.mp4","2026-09-01",&[[53.0,-113.0],[53.01,-113.01]],&[4,4],&[1,1],&[25.0,26.0],&[0.5,0.6],0,60,&[],&[]).unwrap();
    let before=store.mutable_route_source_revision().unwrap();
    store.with_locked_conn(|conn| -> Result<()> {
        let tx=conn.unchecked_transaction()?;
        tx.execute("UPDATE routes SET cloud_uploaded_at=1",[])?;
        tx.rollback()?;
        Ok(())
    }).unwrap();
    assert_eq!(store.mutable_route_source_revision().unwrap(),before);
    store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_uploaded_at=1",[])).unwrap();
    let updated=store.mutable_route_source_revision().unwrap();assert!(updated>before);
    store.with_locked_conn(|conn|conn.execute("DELETE FROM routes",[])).unwrap();
    assert!(store.mutable_route_source_revision().unwrap()>updated);
}

#[test]
fn charging_receipt_confirmation_commits_fields_queue_and_receipt_together() {
    let store=DriveStore::open_memory().unwrap();
    store.charge_upload_mark(100,"charge","key",1).unwrap();
    store.set_charge_tags(100,&["Local".into()]).unwrap();
    let at=store.dirty_mutables().unwrap()[0].2;
    store.with_locked_conn(|c| {
        crate::schema::meta_set(c,"synthetic-publication","exact-request")?;
        c.execute_batch("CREATE TRIGGER fail_receipt_delete BEFORE DELETE ON meta WHEN OLD.key='synthetic-publication' BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;")?;
        Ok::<_,anyhow::Error>(())
    }).unwrap();
    assert!(store.confirm_charge_mutable_push_with_receipt(100,at,"charge","key",1,
        &["Local".into(),"Remote".into()],Some((4.0,"CAD".into())),Some(("synthetic-publication","exact-request"))).is_err());
    assert_eq!(store.get_charge_tags(100).unwrap(),vec!["Local"]);assert_eq!(store.get_charge_cost(100).unwrap(),None);
    assert_eq!(store.dirty_mutables().unwrap().len(),1);
    assert_eq!(store.with_locked_conn(|c| crate::schema::meta_get(c,"synthetic-publication")).unwrap().as_deref(),Some("exact-request"));
    store.with_locked_conn(|c|c.execute_batch("DROP TRIGGER fail_receipt_delete")).unwrap();
    assert!(!store.confirm_charge_mutable_push_with_receipt(100,at,"charge","key",1,
        &[],None,Some(("synthetic-publication","another-request"))).unwrap());
    assert!(store.confirm_charge_mutable_push_with_receipt(100,at,"charge","key",1,
        &["Local".into(),"Remote".into()],Some((4.0,"CAD".into())),Some(("synthetic-publication","exact-request"))).unwrap());
    assert_eq!(store.get_charge_tags(100).unwrap(),vec!["Local","Remote"]);
    assert_eq!(store.get_charge_cost(100).unwrap(),Some((4.0,"CAD".into())));
    assert!(store.dirty_mutables().unwrap().is_empty());
    assert!(store.with_locked_conn(|c| crate::schema::meta_get(c,"synthetic-publication")).unwrap().is_none());
}

#[test]
fn duplicate_charge_acknowledges_existence_without_a_new_key_or_retiring_mutable_intent() {
    let store=DriveStore::open_memory().unwrap();
    store.set_charge_tags(100,&["Local".into()]).unwrap();
    let before=store.charge_mutable_for_upload(100).unwrap();
    assert!(store.confirm_charge_upload(100,"charge",None,1,before.changed_at).unwrap());
    let pending=store.charge_mutable_for_sync(100,before.changed_at.unwrap()).unwrap().unwrap();
    assert_eq!(pending.upload,Some(("charge".into(),String::new(),1)));
    assert_eq!(pending.tags,vec!["Local"]);assert_eq!(pending.intent.edits.len(),1);
    assert!(store.backfill_charge_upload_key(100,"charge",1,"authenticated-key").unwrap());
    assert!(store.confirm_charge_upload(100,"charge",None,2,before.changed_at).unwrap());
    assert_eq!(store.charge_mutable_for_sync(100,before.changed_at.unwrap()).unwrap().unwrap().upload,
        Some(("charge".into(),"authenticated-key".into(),1)));
    assert!(!store.backfill_charge_upload_key(100,"charge",1,"different-key").unwrap());
}

#[test]
fn stored_charge_upload_retires_only_its_prepared_generation_atomically() {
    let store=DriveStore::open_memory().unwrap();
    store.set_charge_tags(100,&["Sent".into()]).unwrap();
    let prepared=store.charge_mutable_for_upload(100).unwrap();
    store.set_charge_tags(100,&["Sent".into(),"Later".into()]).unwrap();
    assert!(store.confirm_charge_upload(100,"charge",Some("stored-key"),1,prepared.changed_at).unwrap());
    let current=store.dirty_mutables().unwrap()[0].2;
    let pending=store.charge_mutable_for_sync(100,current).unwrap().unwrap();
    assert_eq!(pending.tags,vec!["Later","Sent"]);assert_eq!(pending.intent.edits.len(),1);
    assert!(!store.confirm_charge_upload(100,"charge",Some("another-candidate"),2,Some(current)).unwrap());
    assert_eq!(store.dirty_mutables().unwrap()[0].2,current);
}

#[test]
fn stored_charge_ack_failure_cannot_retire_intent_without_the_upload_marker() {
    let store=DriveStore::open_memory().unwrap();store.set_charge_tags(100,&["Keep".into()]).unwrap();
    let before=store.charge_mutable_for_upload(100).unwrap();
    store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_charge_marker BEFORE INSERT ON charge_uploads BEGIN SELECT RAISE(ABORT,'synthetic marker failure'); END;")).unwrap();
    assert!(store.confirm_charge_upload(100,"charge",Some("key"),1,before.changed_at).is_err());
    assert_eq!(store.dirty_mutables().unwrap()[0].2,before.changed_at.unwrap());
    assert_eq!(store.get_charge_tags(100).unwrap(),vec!["Keep"]);
    assert!(store.charge_mutable_for_sync(100,before.changed_at.unwrap()).unwrap().unwrap().upload.is_none());
}

#[test]
fn unreadable_upload_mutables_do_not_turn_into_an_empty_initial_upload() {
    let store=DriveStore::open_memory().unwrap();
    store.with_locked_conn(|conn|conn.execute("INSERT INTO charge_tags(session_ts,tag) VALUES(100,X'FF')",[])).unwrap();
    assert!(store.charge_mutable_for_upload(100).is_err());
}

#[test]
fn drive_receipt_confirmation_preserves_newer_intent_and_commits_key_cache_together() {
    let store=DriveStore::open_memory().unwrap();let file="2026-09-01_10-00-00-front.mp4";
    store.add_route(file,"2026-09-01",&[[50.0,-110.0],[50.01,-110.01]],&[1,1],&[1,1],&[20.0,20.0],&[],0,60,&[],&[]).unwrap();
    store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_route_id='source',cloud_uploaded_at=1 WHERE file=?1",[file])).unwrap();
    let sources=[VerifiedRouteSyncKey {file:file.into(),route_id:"source".into(),expected_key:None,uploaded_at:1,wrapped_key:"authenticated-key".into()}];
    store.set_drive_tags("drive",&["Work".into()]).unwrap();let through=store.dirty_mutables().unwrap()[0].2;
    store.with_locked_conn(|conn|schema::meta_set(conn,"synthetic-drive-receipt","exact")).unwrap();
    store.set_drive_tags("drive",&["Later".into()]).unwrap();
    let revision=store.mutable_route_source_revision().unwrap();
    assert!(!store.confirm_drive_mutable_push_with_receipt("drive",through,revision,&["Remote".into()],&sources,Some(("synthetic-drive-receipt","wrong"))).unwrap());
    store.with_locked_conn(|conn|conn.execute_batch("CREATE TRIGGER fail_drive_receipt BEFORE DELETE ON meta WHEN OLD.key='synthetic-drive-receipt' BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;")).unwrap();
    assert!(store.confirm_drive_mutable_push_with_receipt("drive",through,revision,&["Remote".into(),"Work".into()],&sources,Some(("synthetic-drive-receipt","exact"))).is_err());
    assert_eq!(store.get_drive_tags("drive").unwrap(),vec!["Later"]);
    assert!(store.route_sync_info_by_cloud_id("source").unwrap().unwrap().1.is_none());
    assert_eq!(store.with_locked_conn(|conn|mutable_intent::read(conn,"drive","drive")).unwrap().edits.len(),2);
    store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_drive_receipt")).unwrap();
    assert!(store.confirm_drive_mutable_push_with_receipt("drive",through,revision,&["Remote".into(),"Work".into()],&sources,Some(("synthetic-drive-receipt","exact"))).unwrap());
    assert_eq!(store.get_drive_tags("drive").unwrap(),vec!["Later","Remote"]);
    assert_eq!(store.route_sync_info_by_cloud_id("source").unwrap().unwrap().1.as_deref(),Some("authenticated-key"));
    assert_eq!(store.with_locked_conn(|conn|mutable_intent::read(conn,"drive","drive")).unwrap().edits.len(),1);
    assert!(store.with_locked_conn(|conn|schema::meta_get(conn,"synthetic-drive-receipt")).unwrap().is_none());
}

#[test]
fn critical_publication_uses_full_sync_and_restores_the_original_mode_on_error() {
    let store=DriveStore::open_memory().unwrap();
    store.with_locked_conn(|conn|conn.pragma_update(None,"synchronous",1)).unwrap();
    let mode=||store.with_locked_conn(|conn|conn.query_row("PRAGMA synchronous",[],|row|row.get::<_,i64>(0))).unwrap();
    store.with_durable_conn(|conn| {
        assert_eq!(conn.query_row("PRAGMA synchronous",[],|row|row.get::<_,i64>(0))?,2);
        let tx=conn.transaction()?;schema::meta_set(&tx,"synthetic-durable","committed")?;tx.commit()?;Ok(())
    }).unwrap();
    assert_eq!(mode(),1);
    assert!(store.with_durable_conn(|conn| -> Result<()> {
        assert_eq!(conn.query_row("PRAGMA synchronous",[],|row|row.get::<_,i64>(0))?,2);
        let tx=conn.transaction()?;schema::meta_set(&tx,"synthetic-durable","should roll back")?;
        anyhow::bail!("synthetic failure")
    }).is_err());
    assert_eq!(mode(),1);
    assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,"synthetic-durable")).unwrap().as_deref(),Some("committed"));
    store.with_locked_conn(|conn|conn.pragma_update(None,"synchronous",3)).unwrap();
    store.with_durable_conn(|conn| {
        assert_eq!(conn.query_row("PRAGMA synchronous",[],|row|row.get::<_,i64>(0))?,3);Ok(())
    }).unwrap();assert_eq!(mode(),3);
}

#[test]
fn user_edits_are_flushed_before_acknowledgement_without_changing_default_telemetry_mode() {
    let store=DriveStore::open_memory().unwrap();
    store.with_locked_conn(|conn|conn.execute_batch("PRAGMA synchronous=NORMAL; CREATE TRIGGER require_edit_sync BEFORE INSERT ON mutable_dirty BEGIN SELECT CASE WHEN (SELECT synchronous FROM pragma_synchronous)<2 THEN RAISE(ABORT,'user edit was not flushed') END; END;")).unwrap();
    store.set_drive_tags("drive",&["Work".into()]).unwrap();
    store.set_charge_tags(1,&["Work".into()]).unwrap();
    store.set_charge_cost(1,Some((5.0,"CAD".into()))).unwrap();
    store.add_charge_tag_bulk(&[1,2],"Trip").unwrap();
    store.mark_rate_config_dirty().unwrap();
    assert_eq!(store.with_locked_conn(|conn|conn.query_row("PRAGMA synchronous",[],|row|row.get::<_,i64>(0))).unwrap(),1);
}

#[test]
fn current_group_projection_and_original_receipt_retire_atomically() {
    let store=DriveStore::open_memory().unwrap();
    store.set_drive_tags("old",&["Work".into()]).unwrap();
    let through=store.dirty_mutables().unwrap()[0].2;
    store.set_drive_tags_from_sync("current",&["Before".into()]).unwrap();
    store.set_drive_tags("other",&["Local pending".into()]).unwrap();
    let source=store.mutable_route_source_revision().unwrap();
    store.with_locked_conn(|conn| {
        schema::meta_set(conn,"scope-test-receipt","exact")?;
        conn.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON drive_tags WHEN NEW.drive_key='current' BEGIN SELECT RAISE(ABORT,'synthetic projection failure'); END;")?;
        Ok::<_,anyhow::Error>(())
    }).unwrap();
    let projection=vec![("current".into(),vec!["Remote current".into()]),("other".into(),vec!["Remote other".into()])];
    assert!(store.confirm_drive_mutable_push_with_projection("old",through,source,&["Confirmed".into()],&[],
        Some(("scope-test-receipt","exact")),&projection).is_err());
    assert_eq!(store.get_drive_tags("old").unwrap(),vec!["Work"]);
    assert_eq!(store.get_drive_tags("current").unwrap(),vec!["Before"]);
    assert!(store.drive_edit_scope("old").unwrap().is_some());
    assert_eq!(store.dirty_mutables().unwrap().len(),2);
    assert_eq!(store.with_locked_conn(|conn|schema::meta_get(conn,"scope-test-receipt")).unwrap().as_deref(),Some("exact"));
    store.with_locked_conn(|conn|conn.execute_batch("DROP TRIGGER fail_projection")).unwrap();
    assert!(store.confirm_drive_mutable_push_with_projection("old",through,source,&["Confirmed".into()],&[],
        Some(("scope-test-receipt","exact")),&projection).unwrap());
    assert_eq!(store.get_drive_tags("current").unwrap(),vec!["Remote current"]);
    assert_eq!(store.get_drive_tags("other").unwrap(),vec!["Local pending"]);
    assert!(store.drive_edit_scope("old").unwrap().is_none());
    assert!(store.drive_edit_scope("other").unwrap().is_some());
    assert_eq!(store.dirty_mutables().unwrap().len(),1);
    assert!(store.with_locked_conn(|conn|schema::meta_get(conn,"scope-test-receipt")).unwrap().is_none());
}

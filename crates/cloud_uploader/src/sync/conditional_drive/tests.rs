use super::*;
use super::super::{revision::tests::fixture,route_pull::tests::route,conditional_charge::tests::server};
const PI_KEY:[u8;32]=[7;32];
const CONTENT_KEY:[u8;32]=[9;32];
fn drive(state:&CloudStateInner,index:usize)->String {
    let mut drives=state.store.with_route_summaries(sentryusb_drives::grouper::drive_key_clip_windows).unwrap();
    drives.sort_by(|a,b|a.0.cmp(&b.0));drives[index].0.clone()
}
fn response(items:&[Value])->Value {json!({"ok":true,"writeProtocol":3,"items":items})}
fn ack(body:&Value,statuses:&[&str])->Value {json!({"ok":true,"writeProtocol":3,"results":body["items"].as_array().unwrap().iter().zip(statuses)
    .map(|(item,status)|json!({"kind":"route","id":item["id"],"status":status})).collect::<Vec<_>>()})}
fn document(item:&Value)->Value {
    encrypt::open_json_b64(&CONTENT_KEY,&aad::route_tags("owner","pi",item["id"].as_str().unwrap()),item["ciphertext"].as_str().unwrap()).unwrap()
}
fn update(remote:&mut Value,item:&Value) {remote["ciphertext"]=item["ciphertext"].clone();remote["tagsFormatVersion"]=item["tagsFormatVersion"].clone();}
fn replace(remote:&mut Value,tags:Value) {
    remote["ciphertext"]=json!(encrypt::seal_json_b64(&CONTENT_KEY,&aad::route_tags("owner","pi",remote["id"].as_str().unwrap()),&tags).unwrap());
    remote["tagsFormatVersion"]=json!(if tags.is_array(){1}else{2});
}

#[tokio::test]
async fn runtime_scoped_write_changes_only_the_selected_drive_frames() {
    let (state,creds)=fixture();let (_,mut remote)=route(&state,0,&[(1,20),(0,5),(1,15),(0,5),(1,15)],
        json!({"version":2,"totalFrames":60,"defaultTags":["Remote"],"spans":[],"extension":{"keep":true}}));
    let selected=drive(&state,1);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (client,task)=server(3,move|path,body,index| {
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(body["items"].as_array().unwrap().len(),1);
            let item=&body["items"][0];let doc=document(item);
            assert_eq!(scoped_route_tags::tags_for_ranges(&doc,60,&[(0,20)]).unwrap(),vec!["Remote"]);
            assert_eq!(scoped_route_tags::tags_for_ranges(&doc,60,&[(25,40)]).unwrap(),vec!["Remote","Work"]);
            assert_eq!(scoped_route_tags::tags_for_ranges(&doc,60,&[(45,60)]).unwrap(),vec!["Remote"]);
            assert_eq!(doc["extension"],json!({"keep":true}));
            assert_eq!(item["expectedSummaryCiphertext"],remote["summaryCiphertext"]);
            update(&mut remote,item);return(200,ack(&body,&["applied"]))
        }
        assert_eq!(path,"/api/pi/sync/state");(200,response(&[remote.clone()]))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Remote","Work"]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert!(state.store.drive_edit_scope(&selected).unwrap().is_none());
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(&revision::binding(&creds).unwrap(),&selected))).unwrap().is_none());
}

#[tokio::test]
async fn partial_save_retries_only_unfinished_members_and_preserves_later_local_edits() {
    let (state,creds)=fixture();let (_,first)=route(&state,0,&[(1,60)],json!(["First"]));
    let (_,second)=route(&state,1,&[(1,60)],json!(["Second"]));let mut remotes=vec![first,second];
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (client,task)=server(5,move|path,body,index| {
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(body["items"].as_array().unwrap().len(),2);
            update(&mut remotes[0],&body["items"][0]);
            replace(&mut remotes[0],json!({"version":2,"totalFrames":60,"defaultTags":["Later Cloud"],"spans":[]}));
            replace(&mut remotes[1],json!(["Second Remote"]));
            return (200,ack(&body,&["applied","conflict"]))
        }
        if index==3 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(body["items"].as_array().unwrap().len(),1);
            assert_eq!(body["items"][0]["id"],remotes[1]["id"]);
            update(&mut remotes[1],&body["items"][0]);return (200,ack(&body,&["applied"]))
        }
        assert_eq!(path,"/api/pi/sync/state");(200,response(&remotes))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());
    state.store.set_drive_tags(&selected,&["New local".into()]).unwrap();
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Later Cloud","New local","Second Remote"]);
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    let remaining=state.store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"drive",&selected)).unwrap();
    assert_eq!(remaining.edits.len(),1);
    assert!(state.store.drive_edit_scope(&selected).unwrap().flatten().is_some());
}

#[test]
fn queued_edit_cannot_expand_to_a_new_member_before_first_publication() {
    let (state,_)=fixture();route(&state,0,&[(1,60)],json!([]));
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let scope=state.store.drive_edit_scope(&selected).unwrap().unwrap().unwrap();
    assert_eq!(scope.windows.len(),1);
    let before=state.store.dirty_mutables().unwrap();
    route(&state,1,&[(1,60)],json!([]));
    let pending=route_pull::snapshot_for_drive(&state,&selected).unwrap().unwrap();
    assert_eq!(pending.sources.len(),1);
    assert_eq!(pending.drives[0].1,scope.windows);
    assert!(state.store.set_drive_tags(&selected,&["Changed".into()]).is_err());
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Work"]);
    assert_eq!(state.store.dirty_mutables().unwrap(),before);
    assert_eq!(state.store.drive_edit_scope(&selected).unwrap().flatten(),Some(scope));
}

#[test]
fn first_upload_can_complete_after_an_offline_edit_but_replacements_cannot_rebind_it() {
    let (state,_)=fixture();let (file,remote)=route(&state,0,&[(1,60)],json!([]));
    state.store.with_locked_conn(|conn|conn.execute(
        "UPDATE routes SET cloud_route_id=NULL,cloud_uploaded_at=NULL WHERE file=?1",[&file])).unwrap();
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    assert!(route_pull::snapshot_for_drive(&state,&selected).is_err());
    state.store.with_locked_conn(|conn|conn.execute(
        "UPDATE routes SET cloud_route_id=?1,cloud_uploaded_at=1 WHERE file=?2",
        rusqlite::params![remote["id"].as_str().unwrap(),file])).unwrap();
    assert!(route_pull::snapshot_for_drive(&state,&selected).unwrap().is_some());
    // Changing the frame runs still fails, even when the edit predates upload.
    state.store.with_locked_conn(|conn|conn.execute(
        "UPDATE routes SET gear_runs_blob=?1 WHERE file=?2",
        rusqlite::params![vec![4u8,60,0,0,0],file])).unwrap();
    assert!(route_pull::snapshot_for_drive(&state,&selected).is_err());
}

#[test]
fn assigned_source_identity_cannot_change_under_a_queued_edit() {
    for column in ["cloud_route_id","cloud_uploaded_at","cloud_wrapped_route_key"] {
        let (state,_)=fixture();let (file,remote)=route(&state,0,&[(1,60)],json!([]));
        state.store.with_locked_conn(|conn|conn.execute(
            "UPDATE routes SET cloud_wrapped_route_key=?1 WHERE file=?2",
            rusqlite::params![remote["wrappedKey"].as_str().unwrap(),file])).unwrap();
        let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
        let sql=match column {
            "cloud_route_id"=>"UPDATE routes SET cloud_route_id=printf('%064d',99) WHERE file=?1",
            "cloud_uploaded_at"=>"UPDATE routes SET cloud_uploaded_at=2 WHERE file=?1",
            _=>"UPDATE routes SET cloud_wrapped_route_key='replaced' WHERE file=?1",
        };
        state.store.with_locked_conn(|conn|conn.execute(sql,[&file])).unwrap();
        assert!(route_pull::snapshot_for_drive(&state,&selected).is_err(),"{column}");
        assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Work"]);
    }
}

#[test]
fn scope_failure_rolls_back_tags_queue_and_intent_together() {
    let (state,_)=fixture();route(&state,0,&[(1,60)],json!([]));
    let selected=drive(&state,0);
    state.store.set_drive_tags_from_sync(&selected,&["Before".into()]).unwrap();
    state.store.with_locked_conn(|conn|conn.execute_batch(
        "CREATE TRIGGER fail_scope BEFORE INSERT ON mutable_drive_scope BEGIN SELECT RAISE(ABORT,'synthetic scope failure'); END;"
    )).unwrap();
    assert!(state.store.set_drive_tags(&selected,&["Work".into()]).is_err());
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Before"]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert!(state.store.drive_edit_scope(&selected).unwrap().is_none());
    assert!(state.store.with_locked_conn(|conn|sentryusb_drives::mutable_intent::read(conn,"drive",&selected)).unwrap().edits.is_empty());
}

#[test]
fn unresolved_and_older_queues_are_not_silently_certified_by_a_later_edit() {
    let (state,_)=fixture();
    let selected="2026-09-01T10:00:00.000";
    state.store.set_drive_tags(selected,&["Work".into()]).unwrap();
    assert_eq!(state.store.drive_edit_scope(selected).unwrap(),Some(None));
    route(&state,0,&[(1,60)],json!([]));
    assert!(route_pull::snapshot_for_drive(&state,selected).is_err());
    assert!(state.store.set_drive_tags(selected,&["Later".into()]).is_err());
    // An older build never saved scope evidence. Preserve that absence for
    // explicit reconciliation before changing the grouping policy.
    state.store.with_locked_conn(|conn|conn.execute("DELETE FROM mutable_drive_scope",[])).unwrap();
    state.store.set_drive_tags(selected,&["Later".into()]).unwrap();
    assert!(state.store.drive_edit_scope(selected).unwrap().is_none());
}

#[tokio::test]
async fn unknown_response_reuses_exact_operation_without_overwriting_a_later_cloud_edit() {
    let (state,creds)=fixture();let (_,mut remote)=route(&state,0,&[(1,60)],json!(["Remote"]));
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();let mut original=None;
    let (client,task)=server(4,move|path,body,index| {
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");original=Some(body["items"][0].clone());
            update(&mut remote,&body["items"][0]);
            replace(&mut remote,json!({"version":2,"totalFrames":60,"defaultTags":["Later Cloud"],"spans":[]}));
            return (503,json!({"error":"synthetic lost response"}))
        }
        if index==2 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");assert_eq!(&body["items"][0],original.as_ref().unwrap());
            return (200,ack(&body,&["applied"]))
        }
        assert_eq!(path,"/api/pi/sync/state");(200,response(&[remote.clone()]))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Later Cloud"]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[tokio::test]
async fn unuploaded_member_prevents_a_partial_initial_publication() {
    let (state,creds)=fixture();let (_,first)=route(&state,0,&[(1,60)],json!([]));let (file,_)=route(&state,1,&[(1,60)],json!([]));
    state.store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_uploaded_at=NULL WHERE file=?1",[file])).unwrap();
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let client=CloudClient::new("http://127.0.0.1:1");
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    assert!(state.store.route_sync_info_by_cloud_id(first["id"].as_str().unwrap()).unwrap().unwrap().1.is_none());
}

#[tokio::test]
async fn unreadable_member_key_blocks_the_whole_initial_drive_publication() {
    let (state,creds)=fixture();let (_,first)=route(&state,0,&[(1,60)],json!([]));let (file,second)=route(&state,1,&[(1,60)],json!([]));
    state.store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_wrapped_route_key='broken' WHERE file=?1",[file])).unwrap();
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (client,task)=server(1,move|path,_,_| {assert_eq!(path,"/api/pi/sync/state");(200,response(&[first.clone(),second.clone()]))}).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[tokio::test]
async fn duplicate_member_ack_cannot_confirm_the_drive() {
    let (state,creds)=fixture();let (_,first)=route(&state,0,&[(1,60)],json!([]));let (_,second)=route(&state,1,&[(1,60)],json!([]));
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (client,task)=server(2,move|path,body,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/state");return (200,response(&[first.clone(),second.clone()]))}
        let mut reply=ack(&body,&["applied","applied"]);reply["results"][1]=reply["results"][0].clone();(200,reply)
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    let raw=state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(&revision::binding(&creds).unwrap(),&selected))).unwrap().unwrap();
    let pending:Pending=serde_json::from_str(&raw).unwrap();
    assert!(pending.members.iter().all(|member|member.outcome==Outcome::Pending));
}

#[tokio::test]
async fn new_member_during_publication_keeps_the_original_drive_intent_and_receipt() {
    let (state,creds)=fixture();let (_,mut remote)=route(&state,0,&[(1,60)],json!([]));
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();let changed=state.clone();
    let (client,task)=server(2,move|path,body,index| {
        if index==0 {assert_eq!(path,"/api/pi/sync/state");return (200,response(&[remote.clone()]))}
        update(&mut remote,&body["items"][0]);route(&changed,1,&[(1,60)],json!([]));
        (200,ack(&body,&["applied"]))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
    assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Work"]);
    assert!(state.store.with_locked_conn(|conn|schema::meta_get(conn,&receipt_key(&revision::binding(&creds).unwrap(),&selected))).unwrap().is_some());
}

#[tokio::test]
async fn process_restart_recovers_the_original_operation_from_sqlite() {
    let mut nonce=[0u8;16];ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(),&mut nonce).unwrap();
    let name:String=nonce.iter().map(|byte|format!("{byte:02x}")).collect();
    let directory=std::env::temp_dir().join(format!("sentry-scoped-drive-restart-{name}"));std::fs::create_dir(&directory).unwrap();
    let path=directory.join("synthetic.db");
    let (mut state,creds)=fixture();Arc::get_mut(&mut state).unwrap().store=Arc::new(crate::open_test_store(path.to_str().unwrap()).unwrap());
    let (_,mut remote)=route(&state,0,&[(1,60)],json!(["Remote"]));let selected=drive(&state,0);
    state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();let mut proposal=None;
    let (client,task)=server(4,move|path,body,index| {
        if index==1 {
            proposal=Some(body["items"][0].clone());update(&mut remote,&body["items"][0]);
            return (503,json!({"error":"synthetic lost acknowledgement"}))
        }
        if index==2 {assert_eq!(body["items"][0],proposal.clone().unwrap());return (200,ack(&body,&["applied"]))}
        assert_eq!(path,"/api/pi/sync/state");(200,response(&[remote.clone()]))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());drop(state);
    let (mut restarted,_)=fixture();Arc::get_mut(&mut restarted).unwrap().store=Arc::new(crate::open_test_store(path.to_str().unwrap()).unwrap());
    push(&restarted,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(restarted.store.get_drive_tags(&selected).unwrap(),vec!["Remote","Work"]);
    assert!(restarted.store.dirty_mutables().unwrap().is_empty());
    drop(restarted);std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn long_drive_preserves_all_members_across_bounded_read_and_write_batches() {
    let (state,creds)=fixture();let mut remotes=std::collections::HashMap::new();
    for index in 0..101 {
        let (_,remote)=route(&state,index,&[(1,60)],json!([]));
        remotes.insert(remote["id"].as_str().unwrap().to_string(),remote);
    }
    let selected=drive(&state,0);
    let snapshot=route_pull::snapshot_for_drive(&state,&selected).unwrap().unwrap();assert_eq!(snapshot.sources.len(),101);
    state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (client,task)=server(6,move|path,body,_| {
        let items=body["items"].as_array().unwrap();assert!(items.len()<=100);
        if path=="/api/pi/sync/mutables/v3" {
            for item in items {update(remotes.get_mut(item["id"].as_str().unwrap()).unwrap(),item);}
            return (200,ack(&body,&vec!["applied";items.len()]))
        }
        assert_eq!(path,"/api/pi/sync/state");
        (200,response(&items.iter().map(|item|remotes[item["id"].as_str().unwrap()].clone()).collect::<Vec<_>>()))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["Work"]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[tokio::test]
async fn first_publication_keeps_original_scope_and_refreshes_a_grown_drive() {
    let (state,creds)=fixture();let (_,mut first)=route(&state,0,&[(1,60)],json!(["First"]));
    let selected=drive(&state,0);state.store.set_drive_tags(&selected,&["Work".into()]).unwrap();
    let (_,second)=route(&state,1,&[(1,60)],json!(["Second"]));
    let (client,task)=server(3,move|path,body,index| {
        if index==0 {
            assert_eq!(body["items"].as_array().unwrap().len(),1);
            return (200,response(&[first.clone()]));
        }
        if index==1 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");
            assert_eq!(body["items"].as_array().unwrap().len(),1);
            assert_eq!(body["items"][0]["id"],first["id"]);
            update(&mut first,&body["items"][0]);return (200,ack(&body,&["applied"]));
        }
        assert_eq!(path,"/api/pi/sync/state");
        assert_eq!(body["items"].as_array().unwrap().len(),2);
        (200,response(&[first.clone(),second.clone()]))
    }).await;
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert_eq!(state.store.get_drive_tags(&selected).unwrap(),vec!["First","Second","Work"]);
    // The next explicit edit can select the now-grown drive independently.
    state.store.set_drive_tags(&selected,&["Next".into()]).unwrap();
    assert_eq!(state.store.drive_edit_scope(&selected).unwrap().unwrap().unwrap().windows.len(),2);
}

#[tokio::test]
async fn uncertain_publication_replays_exactly_after_its_drive_key_moves() {
    let (state,creds)=fixture();let (_,mut original)=route(&state,1,&[(1,60)],json!(["Original"]));
    let old_key=drive(&state,0);state.store.set_drive_tags(&old_key,&["Work".into()]).unwrap();
    let changed=state.clone();let mut added=None;let mut proposal=None;
    let (client,task)=server(4,move|path,body,index| {
        if index==0 {return (200,response(&[original.clone()]))}
        if index==1 {
            proposal=Some(body["items"][0].clone());update(&mut original,&body["items"][0]);
            added=Some(route(&changed,0,&[(1,60)],json!(["Earlier clip"])).1);
            return (503,json!({"error":"synthetic lost acknowledgement"}));
        }
        if index==2 {
            assert_eq!(path,"/api/pi/sync/mutables/v3");
            assert_eq!(body["items"].as_array().unwrap().len(),1);
            assert_eq!(&body["items"][0],proposal.as_ref().unwrap());
            return (200,ack(&body,&["applied"]));
        }
        assert_eq!(path,"/api/pi/sync/state");
        assert_eq!(body["items"].as_array().unwrap().len(),2);
        (200,response(&[original.clone(),added.clone().unwrap()]))
    }).await;
    assert!(push(&state,&client,&creds,&PI_KEY).await.is_err());
    let new_key=drive(&state,0);assert_ne!(old_key,new_key);
    push(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(state.store.get_drive_tags(&new_key).unwrap(),vec!["Earlier clip","Original","Work"]);
    assert_eq!(state.store.get_drive_tags(&old_key).unwrap(),vec!["Original","Work"]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
    assert!(state.store.drive_edit_scope(&old_key).unwrap().is_none());
}

use super::*;
use super::super::revision::tests::fixture;
use base64::{Engine as _,engine::general_purpose::STANDARD as B64};
use sentryusb_cloud_crypto::aead;
use sentryusb_drives::types::GearRun;
const PI_KEY:[u8;32]=[7;32];
const CONTENT_KEY:[u8;32]=[9;32];

pub(in crate::sync) fn route(state:&CloudStateInner,second:u8,runs:&[(u8,u32)],tags:Value)->(String,Value) {
    let file=format!("2026-09-01_{:02}-{:02}-00-front.mp4",10+second/60,second%60);
    let id=format!("{:064x}",u32::from(second)+1);
    let gears:Vec<u8>=runs.iter().flat_map(|(gear,frames)|vec![*gear;*frames as usize]).collect();
    let len=gears.len();
    let points:Vec<_>=(0..len).map(|i|[50.0+i as f64*0.00001,-110.0]).collect();
    let gear_runs:Vec<_>=runs.iter().map(|(gear,frames)|GearRun {gear:*gear,frames:*frames}).collect();
    state.store.add_route(&file,"2026-09-01",&points,&gears,&vec![1;len],&vec![20.0;len],&vec![0.0;len],
        gears.iter().filter(|&&g|g==0).count() as u32,len as u32,&gear_runs,&[]).unwrap();
    state.store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_route_id=?1,cloud_uploaded_at=1 WHERE file=?2",
        rusqlite::params![id,file])).unwrap();
    let wrapped=B64.encode(aead::seal(&aead::Key::from_bytes(&PI_KEY).unwrap(),&aad::route_key("owner","pi",&id),&CONTENT_KEY).unwrap());
    let ct=encrypt::seal_json_b64(&CONTENT_KEY,&aad::route_tags("owner","pi",&id),&tags).unwrap();
    let summary=json!({"file":file,"gr":runs.iter().flat_map(|(gear,frames)|[u32::from(*gear),*frames]).collect::<Vec<_>>()});
    let summary=encrypt::seal_json_b64(&CONTENT_KEY,&aad::route_summary("owner","pi",&id),&summary).unwrap();
    (file,json!({"status":"ok","kind":"route","id":id,"recordVersion":"a".repeat(64),"wrappedKey":wrapped,
        "ciphertext":ct,"summaryCiphertext":summary,"tagsFormatVersion":if tags.is_array(){1}else{2},"updatedAtMs":100}))
}
fn remotes(values:Vec<Value>)->HashMap<String,Remote> {
    values.into_iter().map(|value|(value["id"].as_str().unwrap().to_string(),serde_json::from_value(value).unwrap())).collect()
}
fn changed(value:&Value)->Vec<String> {vec![value["id"].as_str().unwrap().to_string()]}
fn tags(state:&CloudStateInner)->Vec<Vec<String>> {
    let mut drives=state.store.with_route_summaries(grouper::drive_key_clip_windows).unwrap();
    drives.sort_by(|a,b|a.0.cmp(&b.0));
    drives.iter().map(|(id,_)|state.store.get_drive_tags(id).unwrap()).collect()
}
fn before(state:&CloudStateInner) {
    let drives=state.store.with_route_summaries(grouper::drive_key_clip_windows).unwrap();
    for (drive,_) in drives {state.store.set_drive_tags_from_sync(&drive,&["Before".into()]).unwrap();}
}

#[test]
fn shared_clip_projects_three_independent_drives_including_empty_windows() {
    let (state,creds)=fixture();
    let (_,remote)=route(&state,0,&[(1,20),(0,5),(1,15),(0,5),(1,15)],
        json!({"version":2,"totalFrames":60,"defaultTags":["Work"],"spans":[
            {"startFrame":0,"endFrame":20,"tags":[]},
            {"startFrame":45,"endFrame":60,"tags":["Personal"]}]}));
    before(&state);
    let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
    assert_eq!(snapshot.drives.len(),3);
    project(snapshot,remotes(vec![remote]),&creds,&PI_KEY).unwrap().apply(&state).unwrap();
    assert_eq!(tags(&state),vec![Vec::<String>::new(),vec!["Work".into()],vec!["Personal".into()]]);
    assert!(state.store.dirty_mutables().unwrap().is_empty());
}

#[test]
fn one_changed_clip_preserves_tags_from_other_members_of_the_same_drive() {
    let (state,creds)=fixture();
    let (_,first)=route(&state,0,&[(1,60)],json!([]));
    let (_,second)=route(&state,1,&[(1,60)],json!(["Keep from other clip"]));
    before(&state);
    let snapshot=snapshot(&state,&changed(&first)).unwrap().unwrap();
    assert_eq!(snapshot.drives.len(),1);assert_eq!(snapshot.sources.len(),2);
    project(snapshot,remotes(vec![first,second]),&creds,&PI_KEY).unwrap().apply(&state).unwrap();
    assert_eq!(tags(&state),vec![vec!["Keep from other clip"]]);
}

#[test]
fn local_edit_during_preparation_survives_any_remote_timestamp() {
    let (state,creds)=fixture();let (_,remote)=route(&state,0,&[(1,60)],json!(["Cloud"]));
    let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
    let drive=snapshot.drives[0].0.clone();
    let projection=project(snapshot,remotes(vec![remote]),&creds,&PI_KEY).unwrap();
    state.store.set_drive_tags(&drive,&["Local".into()]).unwrap();
    projection.apply(&state).unwrap();
    assert_eq!(tags(&state),vec![vec!["Local"]]);assert_eq!(state.store.dirty_mutables().unwrap().len(),1);
}

#[test]
fn source_change_during_network_read_cannot_publish_tags_or_keys() {
    let (state,creds)=fixture();let (file,remote)=route(&state,0,&[(1,60)],json!(["Cloud"]));before(&state);
    let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
    let projection=project(snapshot,remotes(vec![remote.clone()]),&creds,&PI_KEY).unwrap();
    state.store.with_locked_conn(|conn|conn.execute("UPDATE routes SET cloud_uploaded_at=2 WHERE file=?1",[file])).unwrap();
    assert!(projection.apply(&state).is_err());
    assert_eq!(tags(&state),vec![vec!["Before"]]);
    assert!(state.store.route_sync_info_by_cloud_id(remote["id"].as_str().unwrap()).unwrap().unwrap().1.is_none());
}

#[test]
fn unreadable_missing_or_changed_members_never_become_empty_success() {
    for problem in ["wrong_key","bad_tags","missing_member","wrong_frames","wrong_file","wrong_format"] {
        let (state,creds)=fixture();let (_,mut remote)=route(&state,0,&[(1,60)],
            json!({"version":2,"totalFrames":60,"defaultTags":["Cloud"],"spans":[]}));before(&state);
        let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
        match problem {
            "bad_tags"=>remote["ciphertext"]=json!("bad"),
            "missing_member"=>{remote=json!({"status":"not_found","kind":"route","id":remote["id"]});},
            "wrong_format"=>remote["tagsFormatVersion"]=json!(1),
            "wrong_frames"|"wrong_file"=> {
                let summary=json!({"file":if problem=="wrong_file" {"different.mp4"} else {"2026-09-01_10-00-00-front.mp4"},
                    "gr":if problem=="wrong_frames" {vec![1,30,0,30]} else {vec![1,60]}});
                remote["summaryCiphertext"]=json!(encrypt::seal_json_b64(&CONTENT_KEY,
                    &aad::route_summary("owner","pi",remote["id"].as_str().unwrap()),&summary).unwrap());
            },
            _=>{},
        }
        let key=if problem=="wrong_key" {[8;32]} else {PI_KEY};
        assert!(project(snapshot,remotes(vec![remote]),&creds,&key).is_err(),"{problem}");
        assert_eq!(tags(&state),vec![vec!["Before"]]);
    }
}

#[test]
fn failed_tag_transaction_rolls_back_all_key_backfills_and_drive_changes() {
    let (state,creds)=fixture();let (_,remote)=route(&state,0,&[(1,20),(0,5),(1,35)],json!(["Cloud"]));before(&state);
    let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
    let projection=project(snapshot,remotes(vec![remote.clone()]),&creds,&PI_KEY).unwrap();
    state.store.with_locked_conn(|conn|conn.execute_batch(
        "CREATE TRIGGER fail_projection BEFORE INSERT ON drive_tags BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;"
    )).unwrap();
    assert!(projection.apply(&state).is_err());
    assert_eq!(tags(&state),vec![vec!["Before"],vec!["Before"]]);
    assert!(state.store.route_sync_info_by_cloud_id(remote["id"].as_str().unwrap()).unwrap().unwrap().1.is_none());
}

#[test]
fn null_tags_still_require_the_correct_wrapping_key() {
    for correct in [true,false] {
        let (state,creds)=fixture();let (_,mut remote)=route(&state,0,&[(1,60)],json!(["Cloud"]));before(&state);
        remote["ciphertext"]=Value::Null;
        let snapshot=snapshot(&state,&changed(&remote)).unwrap().unwrap();
        let key=if correct {PI_KEY} else {[8;32]};
        let result=project(snapshot,remotes(vec![remote.clone()]),&creds,&key);
        assert_eq!(result.is_ok(),correct);
        if let Ok(projection)=result {projection.apply(&state).unwrap();}
        assert_eq!(tags(&state),if correct {vec![Vec::<String>::new()]} else {vec![vec!["Before".into()]]});
        assert_eq!(state.store.route_sync_info_by_cloud_id(remote["id"].as_str().unwrap()).unwrap().unwrap().1.is_some(),correct);
    }
}

#[tokio::test]
async fn revision_runtime_reads_all_members_before_applying_a_shared_drive() {
    let (state,creds)=fixture();
    let (_,first)=route(&state,0,&[(1,60)],json!(["First"]));
    let (_,second)=route(&state,1,&[(1,60)],json!(["Second"]));before(&state);
    let feed=json!({"ok":true,"items":[{"kind":"route","id":first["id"],"revision":"1",
        "updatedAtMs":100,"ciphertext":first["ciphertext"],"wrappedKey":first["wrappedKey"]}],
        "nextCursor":null,"sync":{"version":2,"revision":"1"}});
    let states=json!({"ok":true,"writeProtocol":3,"items":[first,second]});
    let (client,paths,task)=super::super::revision::tests::server(vec![(200,feed),(200,states)],|_|async{}).await;
    revision::pull_changes(&state,&client,&creds,&PI_KEY).await.unwrap();task.await.unwrap();
    assert_eq!(tags(&state),vec![vec!["First","Second"]]);
    assert_eq!(paths.lock().unwrap().as_slice(),&["/api/pi/sync/changes/v2?limit=200","/api/pi/sync/state"]);
    assert!(state.store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,"cloud_mutable_revision_checkpoint_v2")).unwrap().is_some());
}

#[tokio::test]
async fn late_targeted_reply_cannot_apply_after_pairing_or_feed_progress_changes() {
    for pairing_change in [true,false] {
        let (state,creds)=fixture();let (_,remote)=route(&state,0,&[(1,60)],json!(["Cloud"]));before(&state);
        let feed=json!({"ok":true,"items":[{"kind":"route","id":remote["id"],"revision":"1",
            "updatedAtMs":100,"ciphertext":remote["ciphertext"],"wrappedKey":remote["wrappedKey"]}],
            "nextCursor":null,"sync":{"version":2,"revision":"1"}});
        let states=json!({"ok":true,"writeProtocol":3,"items":[remote]});let changed=state.clone();
        let (client,_,task)=super::super::revision::tests::server(vec![(200,feed),(200,states)],move|index| {
            let changed=changed.clone();async move {
                if index==1 {
                    if pairing_change {changed.creds.lock().await.as_mut().unwrap().pi_id="replacement-pi".into();}
                    else {changed.store.with_locked_conn(|conn|sentryusb_drives::schema::meta_set(conn,"cloud_mutable_revision_walk_v2","newer-progress")).unwrap();}
                }
            }
        }).await;
        assert!(revision::pull_changes(&state,&client,&creds,&PI_KEY).await.is_err());task.await.unwrap();
        assert_eq!(tags(&state),vec![vec!["Before"]]);
        assert!(state.store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,"cloud_mutable_revision_checkpoint_v2")).unwrap().is_none());
    }
}

#[tokio::test]
async fn incomplete_targeted_states_preserve_tags_and_keys_with_a_durable_retry() {
    for problem in ["missing_member","missing_nullable","duplicate_id","foreign_id","wrong_format"] {
        let (state,creds)=fixture();let (_,first)=route(&state,0,&[(1,60)],json!(["First"]));
        let (_,second)=route(&state,1,&[(1,60)],json!(["Second"]));before(&state);
        let feed=json!({"ok":true,"items":[{"kind":"route","id":first["id"],"revision":"1",
            "updatedAtMs":100,"ciphertext":first["ciphertext"],"wrappedKey":first["wrappedKey"]}],
            "nextCursor":null,"sync":{"version":2,"revision":"1"}});
        let mut items=vec![first.clone(),second];
        match problem {
            "missing_member"=>{items.pop();},
            "missing_nullable"=>{items[0].as_object_mut().unwrap().remove("ciphertext");},
            "duplicate_id"=>items[1]=items[0].clone(),
            "foreign_id"=>items[1]["id"]=json!("f".repeat(64)),
            "wrong_format"=>items[0]["tagsFormatVersion"]=json!(3),
            _=>unreachable!(),
        }
        let states=json!({"ok":true,"writeProtocol":3,"items":items});
        let (client,_,task)=super::super::revision::tests::server(vec![(200,feed),(200,states)],|_|async{}).await;
        assert!(revision::pull_changes(&state,&client,&creds,&PI_KEY).await.is_err(),"{problem}");task.await.unwrap();
        assert_eq!(tags(&state),vec![vec!["Before"]]);
        assert!(state.store.route_sync_info_by_cloud_id(first["id"].as_str().unwrap()).unwrap().unwrap().1.is_none());
        assert!(state.store.with_locked_conn(|conn|sentryusb_drives::schema::meta_get(conn,"cloud_mutable_revision_checkpoint_v2")).unwrap().is_some());
        assert_eq!(state.store.with_locked_conn(|conn|crate::sync::incoming_retry::count(conn,&revision::binding(&creds).unwrap())).unwrap(),1);
    }
}

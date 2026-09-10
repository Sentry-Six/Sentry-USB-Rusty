//! Optional source-bound raw-frame evidence for summary-only Summon detection.
use serde_json::{json, Value};
use sentryusb_drives::types::Route;

pub(crate) fn attach(summary: &mut Value, route: &Route, user: &str, pi: &str, id: &str, wrapped: &str) {
    if !summary.is_object() || summary.get("se").is_some() || route.flag_runs.is_empty()
        || route.source.as_deref().is_some_and(|s| s != "sei") || route.file.contains("-front-bridge.mp4")
        || user.is_empty() || pi.is_empty() || id.is_empty() || wrapped.is_empty()
        || route.flag_runs.len() > 512 || route.ap_runs.len() > 512 || route.gear_runs.len() > 512 { return; }
    let total = |frames: Vec<u32>| -> Option<u32> {
        frames.into_iter().try_fold(0u32, |sum, n| if n == 0 { None } else { sum.checked_add(n) })
    };
    let Some(n) = total(route.flag_runs.iter().map(|r| r.frames).collect()).filter(|n| *n > 0) else { return; };
    if (route.raw_frame_count != 0 && route.raw_frame_count != n)
        || route.flag_runs.iter().any(|r| r.max_mps.is_some_and(|v| !v.is_finite() || v < 0.0))
        || (!route.ap_runs.is_empty() && total(route.ap_runs.iter().map(|r| r.frames).collect()) != Some(n))
        || (!route.gear_runs.is_empty() && total(route.gear_runs.iter().map(|r| r.frames).collect()) != Some(n)) { return; }
    let gear: Vec<u32> = route.gear_runs.iter().flat_map(|r| [u32::from(r.gear), r.frames]).collect();
    if summary.get("gr") != Some(&json!(gear)) { return; }
    summary["se"] = json!({"v":1,"u":user,"p":pi,"r":id,"w":wrapped,"n":n,
        "f":route.flag_runs.iter().map(|r| (r.flags,r.frames,r.max_mps)).collect::<Vec<_>>(),
        "a":route.ap_runs.iter().map(|r| (r.ap,r.frames)).collect::<Vec<_>>(),
        "g":route.gear_runs.iter().map(|r| (r.gear,r.frames)).collect::<Vec<_>>()});
    // Locations and canonical metrics keep their existing priority. A large
    // optional capsule must not reject an otherwise valid route upload.
    if !serde_json::to_vec(summary).is_ok_and(|bytes| bytes.len()+29 <= 4096) {
        summary.as_object_mut().unwrap().remove("se");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentryusb_drives::types::{FlagRun, ApRun, GearRun};
    fn route() -> Route {
        Route { file:"2026-09-09_12-00-00-front.mp4".into(),raw_frame_count:1000,
            flag_runs:vec![FlagRun {flags:0,frames:1000,max_mps:Some(2.0)}],
            ap_runs:vec![ApRun {ap:0,frames:100},ApRun {ap:1,frames:800},ApRun {ap:0,frames:100}],
            gear_runs:vec![GearRun {gear:0,frames:100},GearRun {gear:1,frames:800},GearRun {gear:0,frames:100}],
            ..Route::default() }
    }
    #[test]
    fn shared_browser_wire_vectors_match_the_native_producer() {
        let vectors:Value=serde_json::from_str(include_str!("../test-support/summon-evidence-v1.json")).unwrap();
        for case in vectors["cases"].as_array().unwrap() {
            let route:Route=serde_json::from_value(case["route"].clone()).unwrap();
            let mut summary=case["summary"].clone();
            attach(&mut summary,&route,"owner","pi","route","wrapped-key");
            assert_eq!(summary["se"],case["expected"],"{}",case["name"]);
        }
    }
    #[test]
    fn copies_original_frame_evidence_and_binds_owner_source_key() {
        let route=route();let mut summary=json!({"gr":[0,100,1,800,0,100],"future":{"keep":true}});
        let before=summary.clone();attach(&mut summary,&route,"owner","pi","route","wrapped-key");
        assert_eq!(summary["se"],json!({"v":1,"u":"owner","p":"pi","r":"route","w":"wrapped-key","n":1000,
            "f":[[0,1000,2.0]],"a":[[0,100],[1,800],[0,100]],"g":[[0,100],[1,800],[0,100]]}));
        summary.as_object_mut().unwrap().remove("se");assert_eq!(summary,before);
    }
    #[test]
    fn rejects_mismatched_frames_and_preserves_unknown_fields_and_size_limit() {
        let mut route=route();let base=json!({"gr":[0,100,1,800,0,100]});
        for bad in [json!({"gr":[]}),json!({"gr":[0,100,1,800,0,100],"se":{"v":99}}),
            json!({"gr":[0,100,1,800,0,100],"padding":"x".repeat(3950)})] {
            let mut value=bad.clone();attach(&mut value,&route,"owner","pi","route","key");assert_eq!(value,bad);
        }
        route.raw_frame_count=999;let mut value=base.clone();attach(&mut value,&route,"owner","pi","route","key");assert_eq!(value,base);
        route.raw_frame_count=1000;route.flag_runs[0].max_mps=Some(f64::NAN);
        attach(&mut value,&route,"owner","pi","route","key");assert_eq!(value,base);
        route.flag_runs[0].max_mps=None;attach(&mut value,&route,"owner","pi","route","key");assert!(value["se"]["f"][0][2].is_null());
    }
}

//! Canonical distance totals from the same aggregate implementation as Rusty.
use serde::{Deserialize, Serialize};
use sentryusb_drives::{aggregate::compute_route_aggregates, types::Route};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMetrics {
    v: u8,
    #[serde(rename = "dM")]
    distance: f64,
    #[serde(rename = "fdM")]
    fsd: f64,
    #[serde(rename = "adM")]
    autosteer: f64,
    #[serde(rename = "tdM")]
    tacc: f64,
    #[serde(deserialize_with = "nullable")]
    ap0: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    first: Option<[f64; 2]>,
    #[serde(deserialize_with = "nullable")]
    last: Option<[f64; 2]>,
}
fn nullable<'de,D,T>(deserializer:D)->Result<Option<T>,D::Error>
where D:serde::Deserializer<'de>,T:Deserialize<'de> { Option::<T>::deserialize(deserializer) }
impl NativeMetrics {
    fn valid(&self) -> bool {
        let point = |value: Option<[f64;2]>| value.is_none_or(|[lat,lng]| lat.is_finite() && lng.is_finite()
            && lat.abs() <= 90.0 && lng.abs() <= 180.0 && !(lat.abs() < 1.0 && lng.abs() < 1.0));
        self.v == 1 && [self.distance,self.fsd,self.autosteer,self.tacc].iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1e9)
            && self.fsd+self.autosteer+self.tacc <= self.distance + (self.distance*1e-10).max(1e-8)
            && self.ap0.is_none_or(|mode| (0..=3).contains(&mode)) && point(self.first) && point(self.last)
    }
}
pub(crate) fn for_route(route: &Route) -> Option<NativeMetrics> {
    if route.source.as_deref().is_some_and(|source| source != "sei") || route.file.contains("-front-bridge.mp4") { return None; }
    let a = compute_route_aggregates(route);
    let value = NativeMetrics {v:1,distance:a.distance_m,fsd:a.fsd_distance_m,autosteer:a.autosteer_distance_m,
        tacc:a.tacc_distance_m,ap0:a.ap_at_start,first:a.start_lat.zip(a.start_lng).map(|(a,b)|[a,b]),
        last:a.end_lat.zip(a.end_lng).map(|(a,b)|[a,b])};
    value.valid().then_some(value)
}
#[cfg(test)]
fn valid(value: &serde_json::Value) -> bool {
    value.is_object() && serde_json::from_value::<NativeMetrics>(value.clone()).is_ok_and(|value| value.valid())
}
/// Preserve unknown extensions and the existing encrypted-summary size limit.
pub(crate) fn attach(summary: &mut serde_json::Value, route: &Route) {
    if !summary.is_object() || summary.get("nm").is_some() { return; }
    let Some(value) = for_route(route) else { return; };
    let Ok(value) = serde_json::to_value(value) else { return; };
    summary["nm"] = value;
    if !serde_json::to_vec(summary).is_ok_and(|bytes| bytes.len()+29 <= 4096) {
        summary.as_object_mut().unwrap().remove("nm");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn shared_browser_documents_have_identical_validation() {
        let cases:serde_json::Value=serde_json::from_str(include_str!("../test-support/native-metrics-v1.json")).unwrap();
        for case in cases.as_array().unwrap() {assert_eq!(valid(&case["value"]),case["valid"].as_bool().unwrap(),"{}",case["name"]);}
    }
    fn route() -> Route {
        Route {file:"2026-09-07_12-00-00-front.mp4".into(),points:vec![[40.0,-73.0],[40.0001,-73.0],[40.0002,-73.0]],
            speeds:vec![2.0,2.0,2.0],autopilot_states:vec![0,1,3],gear_states:vec![4,4,4],..Route::default()}
    }
    #[test]
    fn producer_preserves_canonical_sub_metre_totals_and_opening_mode() {
        let route=route();let a=compute_route_aggregates(&route);let m=for_route(&route).unwrap();
        assert_eq!(m.distance,a.distance_m);assert_eq!(m.fsd,a.fsd_distance_m);assert_eq!(m.tacc,a.tacc_distance_m);
        assert!((m.distance-m.distance.round()).abs()>0.01);assert_eq!(m.ap0,Some(0));
        assert_eq!(m.first,Some(route.points[0]));assert_eq!(m.last,Some(route.points[2]));
        assert!(valid(&serde_json::to_value(m).unwrap()));
    }
    #[test]
    fn optional_extension_does_not_replace_unknown_versions_or_exceed_summary_limit() {
        let route=route();let mut summary=json!({"v":4,"future":true});let before=summary.clone();attach(&mut summary,&route);
        assert!(valid(&summary["nm"]));summary.as_object_mut().unwrap().remove("nm");assert_eq!(summary,before);
        let mut future=json!({"v":4,"nm":{"v":99,"preserve":true}});let before=future.clone();attach(&mut future,&route);assert_eq!(future,before);
        let mut large=json!({"padding":"x".repeat(4000)});let before=large.clone();assert!(serde_json::to_vec(&large).unwrap().len()+29<=4096);
        attach(&mut large,&route);assert_eq!(large,before);
        let mut imported=route.clone();imported.source=Some("tessie".into());assert!(for_route(&imported).is_none());
        imported.source=None;imported.file="date-front-bridge.mp4".into();assert!(for_route(&imported).is_none());
    }
}

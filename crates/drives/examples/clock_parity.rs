//! In-memory clock parity probe. Input originals are never persisted or printed.
use sentryusb_drives::{Route,RouteSummary,aggregate::compute_route_aggregates,grouper};
use serde::Deserialize;
use serde_json::{Value,json};
use std::{collections::HashMap,io::{self,Read}};
#[derive(Deserialize)]
struct Input {sources:Vec<Source>}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Source {original:Route,native_summary:Value,native_ap0:Option<i32>}
fn main()->anyhow::Result<()> {
    let mut input=Vec::new();io::stdin().take(6*1024*1024+1).read_to_end(&mut input)?;
    anyhow::ensure!(input.len()<=6*1024*1024,"source input exceeds bound");
    let input:Input=serde_json::from_slice(&input)?;
    let summaries:Vec<_>=input.sources.iter().map(|source| {
        let route=&source.original;let mut aggregates=compute_route_aggregates(route);
        for (name,destination) in [("dM",&mut aggregates.distance_m),("fdM",&mut aggregates.fsd_distance_m),
            ("adM",&mut aggregates.autosteer_distance_m),("tdM",&mut aggregates.tacc_distance_m)] {
            if let Some(value)=source.native_summary[name].as_f64() {*destination=value;}
        }
        aggregates.ap_at_start=source.native_ap0;
        RouteSummary {file:route.file.clone(),date:route.date.clone(),raw_park_count:route.raw_park_count,
            raw_frame_count:route.raw_frame_count,gear_runs:route.gear_runs.clone(),flag_runs:route.flag_runs.clone(),
            ap_runs:route.ap_runs.clone(),source:route.source.clone(),external_signature:route.external_signature.clone(),
            aggregates,telemetry:Default::default()}
    }).collect();
    let tags=HashMap::new();let listed=grouper::group_summaries_fast(&summaries,&tags);
    let routes:Vec<_>=input.sources.into_iter().map(|source|source.original).collect();
    let previews=grouper::route_overviews(routes.clone(),500);
    let mut result=Vec::new();
    for summary in listed {
        let selected=grouper::resolve_drive_selection(&summaries,&summary.start_time,&tags)
            .ok_or_else(||anyhow::anyhow!("summary selection missing"))?;
        let members=routes.iter().filter(|route|selected.files.contains(&route.file)).cloned().collect::<Vec<_>>();
        let detail=grouper::build_single_drive_from_clips_with_spans(&members,selected.index as i32,&tags,
            Some(&summary.start_time),Some(&selected.clip_spans_ms))
            .ok_or_else(||anyhow::anyhow!("detail selection missing"))?;
        result.push(json!({"startTime":summary.start_time,"durationMs":summary.duration_ms,
            "distanceKm":summary.distance_km,"fsdPercent":summary.fsd_percent,"summon":summary.summon,
            "detailStartMatches":detail.start_time==summary.start_time,
            "detailDurationMs":detail.duration_ms,"detailEndMatches":detail.end_time==summary.end_time,
            "previewStartMatches":previews.iter().any(|preview|preview.start_time==summary.start_time)}));
    }
    println!("{}",json!({"drives":result}));Ok(())
}

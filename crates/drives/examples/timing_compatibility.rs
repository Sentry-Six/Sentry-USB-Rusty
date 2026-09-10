//! Read grouping-only metadata from stdin; print aggregate transition counts.
//! No database open, recording mutation, or identifier/telemetry output.
use sentryusb_drives::{GearRun, RouteAggregates, RouteSummary, grouper::timing_compatibility};
use serde::Deserialize;
use std::{collections::HashSet, io::{self, Read}};

#[derive(Deserialize)]
struct Input {
    rows: Vec<Row>,
    tagged_keys: Vec<String>,
    pending_keys: Vec<String>,
    #[serde(default)]
    tags: std::collections::BTreeMap<String,Vec<String>>,
}
#[derive(Deserialize)]
struct Row {
    file: String,
    date: String,
    raw_park_count: u32,
    raw_frame_count: u32,
    gear_runs: Vec<(u8, u32)>,
    max_speed_mps: Option<f64>,
    source: Option<String>,
    external_signature: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    io::stdin().take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 64 * 1024 * 1024, "grouping input exceeds limit");
    let input: Input = serde_json::from_slice(&bytes)?;
    let summaries: Vec<_> = input.rows.into_iter().map(|row| RouteSummary {
        file: row.file, date: row.date, raw_park_count: row.raw_park_count,
        raw_frame_count: row.raw_frame_count,
        gear_runs: row.gear_runs.into_iter().map(|(gear, frames)| GearRun { gear, frames }).collect(),
        aggregates: RouteAggregates { max_speed_mps: row.max_speed_mps.unwrap_or_default(), ..Default::default() },
        source: row.source, external_signature: row.external_signature,
        flag_runs: vec![], ap_runs: vec![], telemetry: Default::default(),
    }).collect();
    let plan = timing_compatibility::plan(&summaries);
    let tag_plan = sentryusb_drives::drive_tag_migration::plan(&summaries,&input.tags)?;
    let mut changed = HashSet::new();
    let mut membership_changes = HashSet::new();
    let mut second_changes = HashSet::new();
    let mut precision_changes = HashSet::new();
    let mut incoming = vec![0; plan.corrected.len()];
    let mut outgoing = vec![0; plan.legacy.len()];
    for link in &plan.links {
        incoming[link.corrected_index] += 1;
        outgoing[link.legacy_index] += 1;
        let old = &plan.legacy[link.legacy_index];
        let new = &plan.corrected[link.corrected_index];
        if old.windows != new.windows { membership_changes.insert(old.start_time.as_str()); }
        if !new.start_time.starts_with(&format!("{}.", old.start_time)) {
            second_changes.insert(old.start_time.as_str());
        } else if new.start_time != format!("{}.000", old.start_time) {
            precision_changes.insert(old.start_time.as_str());
        }
        if new.start_time != format!("{}.000", old.start_time) || old.windows != new.windows {
            changed.insert(old.start_time.as_str());
        }
    }
    for (index, count) in outgoing.iter().enumerate() {
        if *count != 1 { changed.insert(plan.legacy[index].start_time.as_str()); }
    }
    println!("{}", serde_json::json!({
        "readOnly": true, "summaryRows": summaries.len(),
        "legacyDrives": plan.legacy.len(), "correctedDrives": plan.corrected.len(),
        "changedLegacyKeys": changed.len(),
        "changedMembershipKeys": membership_changes.len(),
        "shiftedWholeSecondKeys": second_changes.len(),
        "fractionalPrecisionKeys": precision_changes.len(),
        "mergedTargets": incoming.iter().filter(|&&n| n > 1).count(),
        "splitSources": outgoing.iter().filter(|&&n| n > 1).count(),
        "unmappedLegacyFrames": plan.unmapped_legacy_frames,
        "addedFrames": plan.added_frames,
        "incompatibleFrameSources": plan.incompatible_frame_sources,
        "affectedTaggedKeys": input.tagged_keys.iter().filter(|key| changed.contains(key.as_str())).count(),
        "affectedPendingKeys": input.pending_keys.iter().filter(|key| changed.contains(key.as_str())).count(),
        "tagMigration": {
            "originalKeysRetained":tag_plan.original_tags.len(),
            "legacyScopesPreserved":tag_plan.legacy.len(),
            "sourceDocuments":tag_plan.sources.len(),
            "correctedTaggedDrives":tag_plan.corrected_tags.values().filter(|tags|!tags.is_empty()).count(),
            "unresolvedKeysRetained":tag_plan.unresolved_keys.len(),
        },
    }));
    Ok(())
}

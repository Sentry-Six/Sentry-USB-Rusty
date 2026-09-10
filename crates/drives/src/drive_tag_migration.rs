//! Lossless tag projection for the actual-span clock transition. Pure planning:
//! installation must atomically retain originals, sources, queues and receipts.
use crate::{RouteSummary, grouper::{DriveClipWindow,timing_compatibility}, scoped_tags};
use anyhow::{Context, Result, ensure};
use serde_json::{Value,json};
use std::collections::{BTreeMap,BTreeSet};

#[derive(Debug, serde::Serialize)]
pub struct LegacyTags {
    pub key: String,
    pub windows: Vec<DriveClipWindow>,
    pub tags: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SourceTags {
    pub runs: Vec<u32>,
    pub total_frames: u32,
    pub document: Value,
}

#[derive(Debug, serde::Serialize)]
pub struct TagMigration {
    /// Retain even unresolved keys and original ordering for recovery/export.
    pub original_tags: BTreeMap<String,Vec<String>>,
    pub legacy: Vec<LegacyTags>,
    pub sources: BTreeMap<String,SourceTags>,
    /// Display projection only. Never upload these unions as whole-clip tags.
    pub corrected_tags: BTreeMap<String,Vec<String>>,
    pub unresolved_keys: Vec<String>,
}

pub fn plan(summaries:&[RouteSummary], tags:&BTreeMap<String,Vec<String>>)->Result<TagMigration> {
    plan_with_scopes(summaries,tags,&BTreeMap::new())
}

/// Frozen queue/receipt windows take precedence over a later legacy regroup.
pub fn plan_with_scopes(summaries:&[RouteSummary],tags:&BTreeMap<String,Vec<String>>,
    overrides:&BTreeMap<String,Vec<DriveClipWindow>>)->Result<TagMigration> {
    let timing=timing_compatibility::plan(summaries);
    ensure!(timing.unmapped_legacy_frames==0 && timing.incompatible_frame_sources==0,
        "clock transition does not preserve original frame membership");
    let summaries:BTreeMap<_,_>=summaries.iter().map(|summary|(summary.file.as_str(),summary)).collect();
    let mut assignments=BTreeMap::<String,Vec<DriveClipWindow>>::new();
    for drive in &timing.legacy {
        assignments.entry(drive.start_time.clone()).or_default().extend(drive.windows.clone());
    }
    for (key,windows) in overrides {assignments.insert(key.clone(),windows.clone());}
    let unresolved_keys=tags.keys().filter(|key|!assignments.contains_key(*key)).cloned().collect();
    let mut legacy=Vec::new();let mut sources=BTreeMap::<String,SourceTags>::new();
    for (key,windows) in &assignments {
        let Some(labels)=tags.get(key) else {continue};
        legacy.push(LegacyTags {key:key.clone(),windows:windows.clone(),tags:labels.clone()});
        for window in windows {
            let source=summaries.get(window.file.as_str()).context("tag source disappeared")?;
            let runs=source.gear_runs.iter().flat_map(|run|[u32::from(run.gear),run.frames]).collect::<Vec<_>>();
            let total=source.gear_runs.iter().try_fold(0u32,|sum,run| {
                ensure!(run.frames>0,"invalid tag source run");
                sum.checked_add(run.frames).context("tag source frame count overflow")
            })?.max(1);
            ensure!(total==window.total_frames,"tag source frame count changed");
            let saved=sources.entry(window.file.clone()).or_insert_with(||SourceTags {
                runs:runs.clone(),total_frames:total,document:json!([]),
            });
            ensure!(saved.runs==runs && saved.total_frames==total,"inconsistent tag source");
            saved.document=scoped_tags::apply_delta(&saved.document,total,
                &[(window.start_frame,window.end_frame)],labels,&[])?;
        }
    }
    // Prove each old view still has exactly its original tag set before using
    // the documents for any new grouping. This catches overlapping ambiguity.
    for (key,windows) in &assignments {
        if !overrides.is_empty() && !tags.contains_key(key) {continue}
        let mut found=BTreeSet::new();
        for window in windows {
            if let Some(source)=sources.get(&window.file) {
                found.extend(scoped_tags::tags_for_ranges(&source.document,source.total_frames,
                    &[(window.start_frame,window.end_frame)])?);
            }
        }
        let expected=tags.get(key).into_iter().flatten().cloned().collect();
        ensure!(found==expected,"legacy tag scopes overlap ambiguously");
    }
    let mut corrected_tags=BTreeMap::<String,Vec<String>>::new();
    for drive in &timing.corrected {
        let mut found=BTreeSet::new();
        for window in &drive.windows {
            if let Some(source)=sources.get(&window.file) {
                ensure!(source.total_frames==window.total_frames,"corrected tag source changed");
                found.extend(scoped_tags::tags_for_ranges(&source.document,source.total_frames,
                    &[(window.start_frame,window.end_frame)])?);
            }
        }
        if let Some(previous)=corrected_tags.get(&drive.start_time) {
            ensure!(previous.iter().cloned().collect::<BTreeSet<_>>()==found,
                "corrected drive keys have different tag scopes");
        }
        corrected_tags.insert(drive.start_time.clone(),found.into_iter().collect());
    }
    Ok(TagMigration {original_tags:tags.clone(),legacy,sources,corrected_tags,unresolved_keys})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GearRun,RouteAggregates};
    fn clip(time:&str,runs:&[(u8,u32)])->RouteSummary {
        RouteSummary {file:format!("/cam/{time}-front.mp4"),date:"2026-01-01".into(),
            raw_park_count:runs.iter().filter(|(g,_)|*g==0).map(|(_,n)|n).sum(),
            raw_frame_count:runs.iter().map(|(_,n)|n).sum(),
            gear_runs:runs.iter().map(|&(gear,frames)|GearRun {gear,frames}).collect(),
            flag_runs:vec![],ap_runs:vec![],aggregates:RouteAggregates::default(),
            source:None,external_signature:None,telemetry:Default::default()}
    }
    #[test]
    fn merged_display_keeps_original_tags_on_distinct_frames() {
        let rows=[clip("2026-01-01_12-00-00",&[(4,20),(0,5),(4,35)]),clip("2026-01-01_12-00-07",&[(0,60)])];
        let tags=BTreeMap::from([("2026-01-01T12:00:00".into(),vec!["Work".into()]),
            ("2026-01-01T12:00:25".into(),vec!["Personal".into()])]);
        let migrated=plan(&rows,&tags).unwrap();
        assert_eq!(migrated.original_tags,tags);assert_eq!(migrated.legacy.len(),2);
        assert_eq!(migrated.corrected_tags["2026-01-01T12:00:00.000"],vec!["Personal","Work"]);
        let source=&migrated.sources[&rows[0].file];
        assert_eq!(scoped_tags::tags_for_ranges(&source.document,60,&[(0,20)]).unwrap(),vec!["Work"]);
        assert!(scoped_tags::tags_for_ranges(&source.document,60,&[(20,25)]).unwrap().is_empty());
        assert_eq!(scoped_tags::tags_for_ranges(&source.document,60,&[(25,60)]).unwrap(),vec!["Personal"]);
        let edited=scoped_tags::apply_delta(&source.document,60,&[(0,60)],&[],&["Work".into()]).unwrap();
        assert!(scoped_tags::tags_for_ranges(&edited,60,&[(0,20)]).unwrap().is_empty());
        assert_eq!(scoped_tags::tags_for_ranges(&edited,60,&[(25,60)]).unwrap(),vec!["Personal"]);
    }
    #[test]
    fn unresolved_keys_are_retained_without_assigning_them_to_another_drive() {
        let rows=[clip("2026-01-01_12-00-00",&[(4,60)])];
        let tags=BTreeMap::from([("unknown legacy key".into(),vec!["Keep".into()])]);
        let migrated=plan(&rows,&tags).unwrap();
        assert_eq!(migrated.original_tags,tags);
        assert_eq!(migrated.unresolved_keys,vec!["unknown legacy key"]);
        assert!(migrated.sources.is_empty());
        assert!(migrated.corrected_tags.values().all(Vec::is_empty));
    }
    #[test]
    fn midnight_shift_retains_tags_and_original_lookup_key() {
        let rows=[clip("2026-01-01_23-59-50",&[(0,20),(4,40)]),clip("2026-01-02_00-00-20",&[(0,60)])];
        let tags=BTreeMap::from([("2026-01-02T00:00:10".into(),vec!["Night".into()])]);
        let migrated=plan(&rows,&tags).unwrap();
        assert_eq!(migrated.legacy[0].key,"2026-01-02T00:00:10");
        assert_eq!(migrated.corrected_tags["2026-01-02T00:00:00.000"],vec!["Night"]);
        assert_eq!(migrated.original_tags,tags);
    }
}

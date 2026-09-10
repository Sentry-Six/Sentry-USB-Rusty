use super::{DriveClipWindow, RouteSummary, group_summary_clips_with_clock};
use std::collections::BTreeMap;

#[derive(Debug, serde::Serialize)]
pub struct PlannedDrive {
    pub start_time: String,
    pub windows: Vec<DriveClipWindow>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct MembershipLink {
    pub legacy_index: usize,
    pub corrected_index: usize,
    pub shared_frames: u64,
}

/// This is a source-window plan, not a timestamp alias table. Several legacy
/// drives may become one corrected drive, and their distinct tag scopes must
/// survive that merge. Callers must not flatten those scopes into replacement
/// whole-drive tags or reinterpret an already queued edit using new windows.
#[derive(Debug, serde::Serialize)]
pub struct TimingPlan {
    pub legacy: Vec<PlannedDrive>,
    pub corrected: Vec<PlannedDrive>,
    pub links: Vec<MembershipLink>,
    pub unmapped_legacy_frames: u64,
    pub added_frames: u64,
    pub incompatible_frame_sources: usize,
}

pub fn plan(summaries: &[RouteSummary]) -> TimingPlan {
    let collect = |actual| {
        group_summary_clips_with_clock(summaries, actual).into_iter().filter_map(|group| {
            let first = group.first()?;
            let start_time = if actual {
                first.timestamp.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
            } else {
                first.timestamp.format("%Y-%m-%dT%H:%M:%S").to_string()
            };
            let windows = group.iter().map(|clip| DriveClipWindow {
                file: clip.summary.file.clone(), start_frame: clip.start_frame,
                end_frame: clip.end_frame, total_frames: clip.total_frames,
            }).collect();
            Some(PlannedDrive { start_time, windows })
        }).collect::<Vec<_>>()
    };
    let legacy = collect(false);
    let corrected = collect(true);
    let mut by_file: BTreeMap<&str, Vec<(usize, &DriveClipWindow)>> = BTreeMap::new();
    for (index, drive) in corrected.iter().enumerate() {
        for window in &drive.windows {
            by_file.entry(&window.file).or_default().push((index, window));
        }
    }
    let mut shared = BTreeMap::<(usize, usize), u64>::new();
    let mut incompatible_frame_sources = 0;
    for (legacy_index, drive) in legacy.iter().enumerate() {
        for old in &drive.windows {
            for &(corrected_index, new) in by_file.get(old.file.as_str()).into_iter().flatten() {
                if old.total_frames != new.total_frames {
                    incompatible_frame_sources += 1;
                    continue;
                }
                let frames = old.end_frame.min(new.end_frame)
                    .saturating_sub(old.start_frame.max(new.start_frame));
                if frames > 0 {
                    *shared.entry((legacy_index, corrected_index)).or_default() += u64::from(frames);
                }
            }
        }
    }
    let shared_total: u64 = shared.values().sum();
    let count = |drives: &[PlannedDrive]| drives.iter().flat_map(|d| &d.windows)
        .map(|w| u64::from(w.end_frame - w.start_frame)).sum::<u64>();
    let unmapped_legacy_frames = count(&legacy).saturating_sub(shared_total);
    let added_frames = count(&corrected).saturating_sub(shared_total);
    let links = shared.into_iter().map(|((legacy_index, corrected_index), shared_frames)|
        MembershipLink { legacy_index, corrected_index, shared_frames }).collect();
    TimingPlan { legacy, corrected, links, unmapped_legacy_frames, added_frames, incompatible_frame_sources }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GearRun, RouteAggregates};

    fn clip(time: &str, runs: &[(u8, u32)]) -> RouteSummary {
        RouteSummary {
            file: format!("/cam/{time}-front.mp4"), date: "2026-01-01".into(),
            raw_park_count: runs.iter().filter(|(g, _)| *g == 0).map(|(_, n)| n).sum(),
            raw_frame_count: runs.iter().map(|(_, n)| n).sum(),
            gear_runs: runs.iter().map(|&(gear, frames)| GearRun { gear, frames }).collect(),
            flag_runs: vec![], ap_runs: vec![], aggregates: RouteAggregates::default(),
            source: None, external_signature: None, telemetry: Default::default(),
        }
    }

    #[test]
    fn full_minutes_keep_exact_membership_and_legacy_keys() {
        let rows = [clip("2026-01-01_12-00-00", &[(4, 20), (0, 5), (4, 35)])];
        let result = plan(&rows);
        assert_eq!(result.legacy.len(), 2);
        assert_eq!(result.corrected.len(), 2);
        assert_eq!(result.legacy[1].start_time, "2026-01-01T12:00:25");
        for index in 0..2 {
            assert_eq!(result.legacy[index].windows, result.corrected[index].windows);
        }
        assert_eq!(result.unmapped_legacy_frames, 0);
        assert_eq!(result.added_frames, 0);
        assert_eq!(result.incompatible_frame_sources, 0);
    }

    #[test]
    fn short_clip_merge_keeps_both_old_scopes_and_identifies_newly_included_frames() {
        let rows = [clip("2026-01-01_12-00-00", &[(4, 20), (0, 5), (4, 35)]),
            clip("2026-01-01_12-00-07", &[(0, 60)])];
        let result = plan(&rows);
        assert_eq!(result.legacy.len(), 2);
        assert_eq!(result.corrected.len(), 1);
        assert_eq!(result.links, vec![
            MembershipLink { legacy_index: 0, corrected_index: 0, shared_frames: 20 },
            MembershipLink { legacy_index: 1, corrected_index: 0, shared_frames: 35 }]);
        assert_eq!(result.unmapped_legacy_frames, 0);
        assert_eq!(result.added_frames, 5);
        assert_eq!(result.corrected[0].windows[0].end_frame, 60);
        // The old second drive starts beyond the seven-second recording.
        // Keep its key in the plan so its tags/queued edits can be preserved.
        assert_eq!(result.legacy[1].start_time, "2026-01-01T12:00:25");
        assert_eq!(result.corrected[0].start_time, "2026-01-01T12:00:00.000");
    }

    #[test]
    fn real_park_boundaries_use_real_offsets_across_midnight() {
        let rows = [clip("2026-01-01_23-59-50", &[(0, 20), (4, 40)]),
            clip("2026-01-02_00-00-20", &[(0, 60)])];
        let result = plan(&rows);
        assert_eq!(result.legacy[0].start_time, "2026-01-02T00:00:10");
        assert_eq!(result.corrected[0].start_time, "2026-01-02T00:00:00.000");
        assert_eq!(result.legacy[0].windows, result.corrected[0].windows);
        assert_eq!(result.unmapped_legacy_frames, 0);
        assert_eq!(result.added_frames, 0);
    }

    #[test]
    fn every_shortened_span_preserves_old_frame_scopes_without_mutating_inputs() {
        let start = chrono::NaiveDateTime::parse_from_str("2026-01-01_23-59-30", "%Y-%m-%d_%H-%M-%S").unwrap();
        for seconds in 1..=60 {
            let next = (start + chrono::Duration::seconds(seconds)).format("%Y-%m-%d_%H-%M-%S").to_string();
            let rows = [clip("2026-01-01_23-59-30", &[(0, 5), (4, 15), (0, 5), (4, 30), (0, 5)]),
                clip(&next, &[(0, 60)])];
            let before = format!("{rows:?}");
            let result = plan(&rows);
            assert_eq!(format!("{rows:?}"), before);
            assert_eq!(result.unmapped_legacy_frames, 0, "span {seconds}");
            assert_eq!(result.incompatible_frame_sources, 0);
            // Assert exact per-file frame coverage independently of the counts.
            for old in result.legacy.iter().flat_map(|drive| &drive.windows) {
                for frame in old.start_frame..old.end_frame {
                    assert_eq!(result.corrected.iter().flat_map(|drive| &drive.windows)
                        .filter(|new| new.file == old.file && new.total_frames == old.total_frames
                            && new.start_frame <= frame && frame < new.end_frame).count(), 1);
                }
            }
        }
    }
}

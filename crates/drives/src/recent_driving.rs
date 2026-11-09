//! "Recent Clips: only while driving" for the archive.
//!
//! Idea and first implementation by Kamil Keski (Sentry-USB-Rusty #263).
//!
//! archiveloop hands over its list of files to archive. For each RecentClips
//! recording, the front camera's Tesla data decides for all of its cameras:
//! out of Park at any point (Drive, Reverse, Neutral, including stops) keeps
//! it, Park the whole time skips it. Everything else in the list passes
//! through untouched. When unsure, keep: a read error, a missing front file,
//! or the newest recording (still being written when the snapshot was taken).
//!
//! Cars without Tesla data (older hardware or software) have none in any clip,
//! so a recording with no data is only skipped when another recording in the
//! same run does have it; otherwise all are kept.
//!
//! Skipped recordings stay on the archive's list until their snapshot goes, so
//! their verdicts are remembered in a small text file to avoid rereading them
//! every run. RecentClips files don't change once written.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Driving,
    Parked,
    NoData,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self { Verdict::Driving => "driving", Verdict::Parked => "parked", Verdict::NoData => "nodata" }
    }
    fn parse(word: &str) -> Option<Self> {
        match word { "driving" => Some(Verdict::Driving), "parked" => Some(Verdict::Parked), "nodata" => Some(Verdict::NoData), _ => None }
    }
}

/// Reads one front camera file.
pub fn front_verdict(path: &Path) -> anyhow::Result<Verdict> {
    let scan = crate::extract::scan_sei_frames(&path.to_string_lossy())?;
    Ok(if scan.gear_frames == 0 { Verdict::NoData } else if scan.out_of_park_frames > 0 { Verdict::Driving } else { Verdict::Parked })
}

/// "RecentClips/2026-11-09_10-00-00-front.mp4" -> "RecentClips/2026-11-09_10-00-00".
/// None for anything that isn't a RecentClips camera file.
fn recording(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("RecentClips/")?;
    let stamp = rest.as_bytes().get(..20)?;
    if rest.contains('/') || !rest.ends_with(".mp4") || stamp[19] != b'-' || !stamp[..19].iter().all(|b| b.is_ascii_digit() || *b == b'-' || *b == b'_') {
        return None;
    }
    Some(&line[..12 + 19])
}

pub struct Outcome {
    pub kept: Vec<String>,
    /// Verdicts to remember: only recordings still on the list.
    pub remembered: BTreeMap<String, Verdict>,
    pub driving: usize,
    pub parked: usize,
    pub no_data: usize,
    pub unsure: usize,
}

/// `known` is what earlier runs remembered; `read` checks one recording's
/// front file (None when it can't be read).
pub fn select(candidates: &str, known: &BTreeMap<String, Verdict>, mut read: impl FnMut(&str) -> Option<Verdict>) -> Outcome {
    let recordings: BTreeSet<&str> = candidates.lines().filter_map(recording).collect();
    let newest = recordings.iter().next_back().copied();
    let mut verdicts = BTreeMap::new();
    let mut remembered = BTreeMap::new();
    let mut unsure = 0;
    for &rec in &recordings {
        let verdict = if Some(rec) == newest { None } else if let Some(&v) = known.get(rec) { Some(v) } else { read(rec) };
        match verdict {
            Some(v) => { verdicts.insert(rec, v); remembered.insert(rec.to_owned(), v); }
            None => unsure += 1,
        }
    }
    let car_has_data = verdicts.values().any(|&v| v != Verdict::NoData);
    let keep = |rec: &str| match verdicts.get(rec) {
        None | Some(Verdict::Driving) => true,
        Some(Verdict::Parked) => false,
        Some(Verdict::NoData) => !car_has_data,
    };
    let kept = candidates.lines().filter(|line| recording(line).is_none_or(keep)).map(str::to_owned).collect();
    let count = |want| verdicts.values().filter(|&&v| v == want).count();
    Outcome { kept, remembered, driving: count(Verdict::Driving), parked: count(Verdict::Parked), no_data: count(Verdict::NoData), unsure }
}

fn read_known(path: &Path) -> BTreeMap<String, Verdict> {
    std::fs::read_to_string(path).unwrap_or_default().lines()
        .filter_map(|line| { let (rec, word) = line.split_once(' ')?; Some((rec.to_owned(), Verdict::parse(word)?)) })
        .collect()
}

/// The `filter-recent-driving` command. Writes the filtered list to `output`
/// and returns a one-line summary for the archive log.
pub fn filter_file(root: &Path, input: &Path, output: &Path, remember: &Path) -> anyhow::Result<String> {
    anyhow::ensure!(input != output, "input and output must differ");
    let candidates = std::fs::read_to_string(input)?;
    let outcome = select(&candidates, &read_known(remember), |rec| front_verdict(&root.join(format!("{rec}-front.mp4"))).ok());
    let mut list = outcome.kept.join("\n");
    if !list.is_empty() { list.push('\n') }
    std::fs::write(output, list)?;
    // Best effort: losing this file only means rereading clips next run.
    let lines: String = outcome.remembered.iter().map(|(rec, v)| format!("{rec} {}\n", v.word())).collect();
    let tmp = remember.with_extension("tmp");
    if std::fs::write(&tmp, lines).is_ok() { let _ = std::fs::rename(&tmp, remember); }
    let skipped = candidates.lines().count() - outcome.kept.len();
    Ok(format!("Recent Clips: {} driving, {} parked, {} without Tesla data, {} kept to be safe; skipping {skipped} files",
        outcome.driving, outcome.parked, outcome.no_data, outcome.unsure))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: &str) -> String { format!("RecentClips/2026-11-09_{ts}") }
    fn cams(ts: &str) -> String {
        ["front", "back", "left_repeater", "right_repeater"].iter().map(|c| format!("{}-{c}.mp4\n", rec(ts))).collect()
    }
    fn run(list: &str, verdicts: &[(&str, Option<Verdict>)]) -> Outcome {
        let map: BTreeMap<String, Option<Verdict>> = verdicts.iter().map(|(ts, v)| (rec(ts), *v)).collect();
        select(list, &BTreeMap::new(), |r| *map.get(r).expect("only listed recordings are read"))
    }
    fn kept_recordings(outcome: &Outcome) -> BTreeSet<String> {
        outcome.kept.iter().filter_map(|l| recording(l).map(str::to_owned)).collect()
    }

    #[test]
    fn driving_keeps_every_camera_and_parked_is_skipped() {
        let list = format!("SentryClips/2026-11-09_09-00-00/event.mp4\n{}{}{}", cams("10-00-00"), cams("10-01-00"), cams("10-02-00"));
        let out = run(&list, &[("10-00-00", Some(Verdict::Driving)), ("10-01-00", Some(Verdict::Parked))]);
        assert_eq!(kept_recordings(&out), [rec("10-00-00"), rec("10-02-00")].into());
        assert_eq!(out.kept.iter().filter(|l| l.starts_with(&rec("10-00-00"))).count(), 4);
        assert!(out.kept.contains(&"SentryClips/2026-11-09_09-00-00/event.mp4".to_owned()), "other folders pass through");
    }

    #[test]
    fn the_newest_recording_is_kept_without_reading_it() {
        let out = run(&format!("{}{}", cams("10-00-00"), cams("10-01-00")), &[("10-00-00", Some(Verdict::Parked))]);
        assert_eq!(kept_recordings(&out), [rec("10-01-00")].into());
        assert_eq!(out.unsure, 1);
    }

    #[test]
    fn unreadable_front_files_are_kept() {
        let out = run(&format!("{}{}", cams("10-00-00"), cams("10-05-00")), &[("10-00-00", None)]);
        assert_eq!(kept_recordings(&out).len(), 2);
        assert!(out.remembered.is_empty(), "nothing to remember about an unreadable file");
    }

    #[test]
    fn no_data_is_skipped_only_when_the_car_has_data_elsewhere() {
        let list = format!("{}{}{}", cams("10-00-00"), cams("10-01-00"), cams("10-02-00"));
        let with_data = run(&list, &[("10-00-00", Some(Verdict::NoData)), ("10-01-00", Some(Verdict::Driving))]);
        assert_eq!(kept_recordings(&with_data), [rec("10-01-00"), rec("10-02-00")].into());
        // An older car: no recording has Tesla data, so nothing is skipped.
        let old_car = run(&list, &[("10-00-00", Some(Verdict::NoData)), ("10-01-00", Some(Verdict::NoData))]);
        assert_eq!(kept_recordings(&old_car).len(), 3);
    }

    #[test]
    fn remembered_verdicts_are_not_read_again_and_stale_ones_drop() {
        let known = BTreeMap::from([(rec("10-00-00"), Verdict::Parked), (rec("08-00-00"), Verdict::Parked)]);
        let out = select(&format!("{}{}", cams("10-00-00"), cams("10-01-00")), &known, |_| panic!("no reads expected"));
        assert_eq!(kept_recordings(&out), [rec("10-01-00")].into());
        assert_eq!(out.remembered.keys().cloned().collect::<Vec<_>>(), vec![rec("10-00-00")]);
    }

    #[test]
    fn filter_file_writes_the_list_and_remembers_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("RecentClips")).unwrap();
        // Unreadable as MP4 -> no Tesla data; with no other data, kept.
        for ts in ["10-00-00", "10-01-00"] { std::fs::write(dir.path().join(format!("{}-front.mp4", rec(ts))), b"not an mp4").unwrap(); }
        let (input, output, remember) = (dir.path().join("in"), dir.path().join("out"), dir.path().join("checked"));
        std::fs::write(&input, format!("{}{}", cams("10-00-00"), cams("10-01-00"))).unwrap();
        let summary = filter_file(dir.path(), &input, &output, &remember).unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap().lines().count(), 8, "{summary}");
        assert!(summary.contains("skipping 0 files"));
    }

    #[test]
    fn only_recentclips_camera_files_are_recordings() {
        assert_eq!(recording("RecentClips/2026-11-09_10-00-00-front.mp4"), Some("RecentClips/2026-11-09_10-00-00"));
        for other in ["SavedClips/2026-11-09_10-00-00/2026-11-09_10-00-00-front.mp4", "RecentClips/../etc-front.mp4", "RecentClips/x.mp4", "RecentClips/sub/2026-11-09_10-00-00-front.mp4"] {
            assert_eq!(recording(other), None, "{other}");
        }
    }
}

//! "Recent Clips: only while driving" for the archive.
//!
//! Idea and first implementation by Kamil Keski (Sentry-USB-Rusty #263).
//!
//! archiveloop hands over its list of files to archive. For each RecentClips
//! recording, the front camera's Tesla data decides for all of its cameras:
//! out of Park at any point (Drive, Reverse, Neutral, including stops) keeps
//! it, Park the whole time skips it. Everything else in the list passes
//! through untouched. When unsure, keep: a read error or a missing front file.
//! Every readable recording is checked, including the newest remaining candidate.
//!
//! Cars without Tesla data (older hardware or software) have none in any clip,
//! so a recording with no data is only skipped after this device has observed
//! Tesla data. Remember that evidence across runs, even after its clip is archived.
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

/// Accept the dated snapshot index and flat RecentClips paths.
/// Return the recording path without its camera suffix.
fn recording(line: &str) -> Option<&str> {
    let (dir, name) = line.rsplit_once('/')?;
    let stamp = name.get(..19)?;
    if name.as_bytes().get(19) != Some(&b'-') || !name.ends_with(".mp4") {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d_%H-%M-%S").ok()?;
    if dir != "RecentClips" && dir.strip_prefix("RecentClips/")? != &stamp[..10] {
        return None;
    }
    Some(&line[..dir.len() + 1 + stamp.len()])
}

#[derive(Default)]
pub struct Known {
    pub verdicts: BTreeMap<String, Verdict>,
    /// Device-level evidence must outlive the candidate that supplied it.
    pub has_data: bool,
}

pub struct Outcome {
    pub kept: Vec<String>,
    /// Verdicts to remember: only recordings still on the list.
    pub remembered: Known,
    pub driving: usize,
    pub parked: usize,
    pub no_data: usize,
    pub unsure: usize,
}

/// `known` is what earlier runs remembered; `read` checks one recording's
/// front file (None when it can't be read).
pub fn select(candidates: &str, known: &Known, mut read: impl FnMut(&str) -> Option<Verdict>) -> Outcome {
    let recordings: BTreeSet<&str> = candidates.lines().filter_map(recording).collect();
    let mut verdicts = BTreeMap::new();
    let mut remembered = BTreeMap::new();
    let mut unsure = 0;
    for &rec in &recordings {
        let verdict = if let Some(&v) = known.verdicts.get(rec) { Some(v) } else { read(rec) };
        match verdict {
            Some(v) => { verdicts.insert(rec, v); remembered.insert(rec.to_owned(), v); }
            None => unsure += 1,
        }
    }
    let car_has_data = known.has_data
        || known.verdicts.values().chain(verdicts.values()).any(|&v| v != Verdict::NoData);
    let keep = |rec: &str| match verdicts.get(rec) {
        None | Some(Verdict::Driving) => true,
        Some(Verdict::Parked) => false,
        Some(Verdict::NoData) => !car_has_data,
    };
    let kept = candidates.lines().filter(|line| recording(line).is_none_or(keep)).map(str::to_owned).collect();
    let count = |want| verdicts.values().filter(|&&v| v == want).count();
    Outcome { kept, remembered: Known { verdicts: remembered, has_data: car_has_data }, driving: count(Verdict::Driving), parked: count(Verdict::Parked), no_data: count(Verdict::NoData), unsure }
}

const HAS_DATA_MARKER: &str = "# tesla-data-seen";

fn read_known(path: &Path) -> Known {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let verdicts: BTreeMap<_, _> = text.lines()
        .filter_map(|line| { let (rec, word) = line.split_once(' ')?; Some((rec.to_owned(), Verdict::parse(word)?)) })
        .collect();
    // Old cache files have no marker, but their gear verdicts are still evidence.
    let has_data = text.lines().any(|line| line == HAS_DATA_MARKER)
        || verdicts.values().any(|&v| v != Verdict::NoData);
    Known { verdicts, has_data }
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
    // Best effort: losing this file requires relearning whether the car emits data.
    let mut lines = if outcome.remembered.has_data { format!("{HAS_DATA_MARKER}\n") } else { String::new() };
    lines.extend(outcome.remembered.verdicts.iter().map(|(rec, v)| format!("{rec} {}\n", v.word())));
    let tmp = remember.with_extension("tmp");
    if std::fs::write(&tmp, lines).is_ok() { let _ = std::fs::rename(&tmp, remember); }
    let skipped = candidates.lines().count() - outcome.kept.len();
    Ok(format!("Recent Clips: {} driving, {} parked, {} without Tesla data, {} kept to be safe; skipping {skipped} files",
        outcome.driving, outcome.parked, outcome.no_data, outcome.unsure))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: &str) -> String { format!("RecentClips/2026-11-09/2026-11-09_{ts}") }
    fn cams(ts: &str) -> String {
        ["front", "back", "left_repeater", "right_repeater", "left_pillar", "right_pillar"].iter()
            .map(|c| format!("{}-{c}.mp4\n", rec(ts))).collect()
    }
    fn run(list: &str, verdicts: &[(&str, Option<Verdict>)]) -> Outcome {
        let map: BTreeMap<String, Option<Verdict>> = verdicts.iter().map(|(ts, v)| (rec(ts), *v)).collect();
        select(list, &Known::default(), |r| *map.get(r).expect("only listed recordings are read"))
    }
    fn kept_recordings(outcome: &Outcome) -> BTreeSet<String> {
        outcome.kept.iter().filter_map(|l| recording(l).map(str::to_owned)).collect()
    }
    fn mp4(gear: Option<u8>) -> Vec<u8> {
        // No GPS or speed: a Tesla SEI gear is sufficient; a video NAL has no SEI.
        let nal = match gear { Some(0) => vec![0x06, 0x05, 0, 0x42, 0x69, 0x08, 1, 0x80], Some(g) => vec![0x06, 0x05, 0, 0x42, 0x69, 0x08, 1, 0x10, g, 0x80], None => vec![0x65, 0x80] };
        let mut bytes = ((12 + nal.len()) as u32).to_be_bytes().to_vec();
        bytes.extend(b"mdat");
        bytes.extend((nal.len() as u32).to_be_bytes());
        bytes.extend(nal);
        bytes
    }

    #[test]
    fn driving_keeps_every_camera_and_parked_is_skipped() {
        let list = format!("SentryClips/event/front.mp4\nSavedClips/event/front.mp4\nTeslaTrackMode/event/front.mp4\n{}{}", cams("10-00-00"), cams("10-01-00"));
        let out = run(&list, &[("10-00-00", Some(Verdict::Driving)), ("10-01-00", Some(Verdict::Parked))]);
        assert_eq!(kept_recordings(&out), [rec("10-00-00")].into());
        assert_eq!(out.kept.len(), 9, "all six driving cameras and all other categories pass through");
    }

    #[test]
    fn newest_candidate_is_checked_and_parked_backlog_never_drains() {
        let list = format!("{}{}{}", cams("10-00-00"), cams("10-01-00"), cams("10-02-00"));
        let mut reads = 0;
        let first = select(&list, &Known::default(), |_| { reads += 1; Some(Verdict::Parked) });
        assert_eq!(reads, 3);
        assert!(first.kept.is_empty());
        let mut known = first.remembered;
        for _ in 0..3 {
            let out = select(&list, &known, |_| panic!("cached Park evidence is authoritative, even for newest"));
            assert!(out.kept.is_empty());
            known = out.remembered;
        }
    }

    #[test]
    fn unreadable_front_files_are_kept_and_retried() {
        let list = cams("10-00-00");
        let out = run(&list, &[("10-00-00", None)]);
        assert_eq!(out.kept.len(), 6);
        assert!(out.remembered.verdicts.is_empty());
        let retry = select(&list, &out.remembered, |_| Some(Verdict::Parked));
        assert!(retry.kept.is_empty());
    }

    #[test]
    fn no_data_is_kept_until_gear_has_been_seen() {
        let list = format!("{}{}", cams("10-00-00"), cams("10-01-00"));
        let with_data = run(&list, &[("10-00-00", Some(Verdict::NoData)), ("10-01-00", Some(Verdict::Driving))]);
        assert_eq!(kept_recordings(&with_data), [rec("10-01-00")].into());
        let old_car = run(&list, &[("10-00-00", Some(Verdict::NoData)), ("10-01-00", Some(Verdict::NoData))]);
        assert_eq!(old_car.kept.len(), 12);
        assert!(!old_car.remembered.has_data);
        let again = select(&list, &old_car.remembered, |_| panic!("reuse no-data evidence"));
        assert_eq!(again.kept.len(), 12);
    }

    #[test]
    fn no_data_stays_skipped_after_driving_candidates_are_archived() {
        let first = run(&(cams("10-00-00") + &cams("10-01-00")), &[("10-00-00", Some(Verdict::NoData)), ("10-01-00", Some(Verdict::Driving))]);
        let second = select(&cams("10-00-00"), &first.remembered, |_| panic!("reuse cached evidence"));
        assert!(second.kept.is_empty());
        assert!(second.remembered.has_data);
        assert_eq!(second.remembered.verdicts.len(), 1, "archived clip is pruned");
        let empty = select("", &second.remembered, |_| panic!("empty list"));
        assert!(empty.remembered.verdicts.is_empty());
        let later = select(&cams("11-00-00"), &empty.remembered, |_| Some(Verdict::NoData));
        assert!(later.kept.is_empty(), "capability evidence outlives all old candidates");
    }

    #[test]
    fn old_text_cache_preserves_capability_when_stale_entries_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checked");
        std::fs::write(&path, format!("{} parked\n", rec("08-00-00"))).unwrap();
        let known = read_known(&path);
        let out = select(&cams("10-00-00"), &known, |_| Some(Verdict::NoData));
        assert!(out.kept.is_empty());
        assert!(out.remembered.has_data);
        assert_eq!(out.remembered.verdicts.keys().cloned().collect::<Vec<_>>(), [rec("10-00-00")]);
    }

    #[test]
    fn filter_file_checks_dated_fronts_and_remembers_data_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("RecentClips/2026-11-09")).unwrap();
        let mut list = String::new();
        for (ts, gear) in [("10-00-00", Some(1)), ("10-01-00", Some(2)), ("10-02-00", Some(3)), ("10-03-00", Some(0)), ("10-04-00", None)] {
            std::fs::write(dir.path().join(format!("{}-front.mp4", rec(ts))), mp4(gear)).unwrap();
            list.push_str(&cams(ts));
        }
        let (input, output, remember) = (dir.path().join("in"), dir.path().join("out"), dir.path().join("checked"));
        std::fs::write(&input, list).unwrap();
        let summary = filter_file(dir.path(), &input, &output, &remember).unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), cams("10-00-00") + &cams("10-01-00") + &cams("10-02-00"));
        assert!(summary.contains("3 driving, 1 parked, 1 without Tesla data"), "{summary}");
        // Next run: only the no-SEI recording still needs an archive decision.
        std::fs::write(&input, cams("10-04-00")).unwrap();
        filter_file(dir.path(), &input, &output, &remember).unwrap();
        assert!(std::fs::read_to_string(&output).unwrap().is_empty());
        let known = read_known(&remember);
        assert!(known.has_data);
        assert_eq!(known.verdicts.len(), 1);
        assert!(std::fs::read_to_string(&remember).unwrap().starts_with(HAS_DATA_MARKER));
        // The front stays authoritative when only a repeater remains a candidate.
        std::fs::write(&input, format!("{}-right_repeater.mp4\n", rec("10-03-00"))).unwrap();
        filter_file(dir.path(), &input, &output, &remember).unwrap();
        assert!(std::fs::read_to_string(&output).unwrap().is_empty());
    }

    #[test]
    fn metadata_free_old_car_and_missing_fronts_are_kept_on_disk_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("RecentClips/2026-11-09")).unwrap();
        std::fs::write(dir.path().join(format!("{}-front.mp4", rec("10-00-00"))), mp4(None)).unwrap();
        let (input, output, remember) = (dir.path().join("in"), dir.path().join("out"), dir.path().join("checked"));
        let list = cams("10-00-00") + &cams("10-01-00"); // second front absent
        std::fs::write(&input, &list).unwrap();
        filter_file(dir.path(), &input, &output, &remember).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), list);
        assert!(!read_known(&remember).has_data);
    }

    #[test]
    fn dated_and_flat_paths_are_recognized_but_unrelated_paths_are_not() {
        for dir in ["RecentClips", "RecentClips/2026-11-09"] {
            let rec = format!("{dir}/2026-11-09_10-00-00");
            assert_eq!(recording(&format!("{rec}-front.mp4")), Some(rec.as_str()));
            let list = format!("{rec}-front.mp4\n{rec}-right_repeater.mp4\n");
            let out = select(&list, &Known::default(), |r| { assert_eq!(r, rec); Some(Verdict::Parked) });
            assert!(out.kept.is_empty());
        }
        for other in ["SavedClips/2026-11-09_10-00-00/2026-11-09_10-00-00-front.mp4", "RecentClips/../2026-11-09_10-00-00-front.mp4", "RecentClips/x.mp4", "RecentClips/sub/2026-11-09_10-00-00-front.mp4", "RecentClips/2026-11-08/2026-11-09_10-00-00-front.mp4", "RecentClips/2026-99-99_10-00-00-front.mp4"] {
            assert_eq!(recording(other), None, "{other}");
        }
    }
}

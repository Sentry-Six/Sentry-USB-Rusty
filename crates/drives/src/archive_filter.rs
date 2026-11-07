//! Archive selection based on the recording car's embedded SEI telemetry.
use crate::extract::{extract_archive_gears, GEAR_PARK};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Component, Path},
    time::{Duration, Instant},
};

/// Apply the front camera's gear verdict to every angle of its recording.
/// Only non-Park front gear qualifies; missing gear data does not. Other
/// categories pass through unchanged.
pub fn filter_driving_recent(root: &Path, candidates: &str) -> Vec<String> {
    filter_with(candidates, |relative| {
        read_evidence(&root.join(relative)).is_ok_and(|evidence| evidence.should_archive())
    })
}

fn recording_key(relative: &str) -> Option<String> {
    let path = Path::new(relative);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if !name.ends_with(".mp4") || name.as_bytes().get(19) != Some(&b'-') {
        return None;
    }
    let timestamp = name.get(..19)?;
    chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d_%H-%M-%S").ok()?;
    Some(format!("{}/{timestamp}", path.parent()?.display()))
}

fn filter_with(candidates: &str, mut driving: impl FnMut(&str) -> bool) -> Vec<String> {
    // Nearby recording timestamps usually share a snapshot. Random hash-map
    // iteration revisits automounts and scatters reads across backing images.
    let mut groups = BTreeSet::new();
    for line in candidates
        .lines()
        .filter(|line| line.starts_with("RecentClips/"))
    {
        if let Some(key) = recording_key(line) {
            groups.insert(key);
        }
    }
    let mut keep = HashSet::new();
    for key in groups {
        // The front is authoritative even when only associated angles remain
        // in the candidate list. A metadata-free repeater must not override Park.
        if driving(&format!("{key}-front.mp4")) {
            keep.insert(key);
        }
    }
    candidates
        .lines()
        .filter(|line| {
            !line.starts_with("RecentClips/")
                || recording_key(line).is_some_and(|key| keep.contains(&key))
        })
        .map(str::to_owned)
        .collect()
}

/// Cache the gear-only selection verdict and explicit missing-gear status.
#[derive(Clone, Copy)]
struct DrivingEvidence {
    keep: bool,
    no_gear: bool,
}

impl DrivingEvidence {
    fn should_archive(self) -> bool {
        // Older prereleases cached keep=true for metadata-free clips. Apply
        // the current policy to that evidence without rescanning the MP4.
        self.keep && !self.no_gear
    }
}

fn read_evidence(path: &Path) -> Result<DrivingEvidence> {
    let gears = extract_archive_gears(&path.to_string_lossy())?;
    Ok(DrivingEvidence {
        keep: gears.iter().any(|&gear| gear != GEAR_PARK),
        no_gear: gears.is_empty(),
    })
}

// A stat checks for replacements or growing clips without rereading the MP4.
// Include the link destination because the index can switch snapshot sources.
fn source_identity(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    let link = std::fs::read_link(path).ok();
    Ok(format!(
        "{:?}:{}:{}:{}:{}:{}",
        link,
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

fn filter_cached(
    root: &Path,
    candidates: &str,
    database: &Path,
    mut extract: impl FnMut(&Path) -> Result<DrivingEvidence>,
) -> Result<Vec<String>> {
    let db = crate::db::open_connection(database).context("Open shared drive database")?;
    crate::schema::migrate(&db)?;
    // Commit each completed scan durably on this dedicated worker connection.
    db.pragma_update(None, "synchronous", "FULL")?;
    let mut failure = None;
    let mut scanned = 0usize;
    let mut reused = 0usize;
    let (mut non_park, mut park_only, mut no_gear) = (0usize, 0usize, 0usize);
    let mut last_report = Instant::now();
    let selected = filter_with(candidates, |relative| {
        if failure.is_some() {
            return false;
        }
        let path = root.join(relative);
        let identity = match source_identity(&path) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(
                    "RecentClips filter: cannot stat {relative}: {error:#}; will retry next cycle"
                );
                return false;
            }
        };
        let result = (|| -> Result<bool> {
            use rusqlite::OptionalExtension;
            let cached = db
                .query_row(
                    "SELECT keep, no_gear FROM archive_driving_evidence WHERE path=?1 AND identity=?2",
                    rusqlite::params![relative, identity],
                    |row| Ok(DrivingEvidence { keep: row.get(0)?, no_gear: row.get(1)? }),
                )
                .optional()?;
            let evidence = if let Some(evidence) = cached {
                reused += 1;
                evidence
            } else {
                if scanned == 0 || last_report.elapsed() >= Duration::from_secs(15) {
                    tracing::info!(
                        "RecentClips filter: scanned {scanned}, reused {reused}; reading {relative}"
                    );
                    last_report = Instant::now();
                }
                scanned += 1;
                let evidence = match extract(&path) {
                    Ok(evidence) => evidence,
                    Err(error) => {
                        tracing::warn!(
                            "RecentClips filter: cannot read {relative}: {error:#}; will retry next cycle"
                        );
                        return Ok(false);
                    }
                };
                // Do not cache a clip which changed while it was being read.
                if source_identity(&path).ok().as_ref() != Some(&identity) {
                    return Ok(false);
                }
                // Autocommit each result so power loss retains completed scans.
                db.execute("INSERT OR REPLACE INTO archive_driving_evidence (path, identity, keep, no_gear) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![relative, identity, evidence.keep, evidence.no_gear])?;
                evidence
            };
            if evidence.no_gear {
                if no_gear == 0 {
                    tracing::debug!("RecentClips filter: no SEI gear data found in {relative}; skipping this recording");
                }
                no_gear += 1;
            } else if evidence.keep {
                non_park += 1;
            } else {
                park_only += 1;
            }
            Ok(evidence.should_archive())
        })();
        match result {
            Ok(driving) => driving,
            Err(error) => {
                failure = Some(error);
                false
            }
        }
    });
    if let Some(error) = failure {
        return Err(error.context("Update archive driving cache"));
    }
    // Keep negative verdicts while candidates exist, but bound growth as the
    // archive ledger and snapshot retention remove old candidates.
    let candidates: HashSet<String> = candidates
        .lines()
        .filter(|line| line.starts_with("RecentClips/"))
        .filter_map(recording_key)
        .map(|key| format!("{key}-front.mp4"))
        .collect();
    let paths = {
        let mut query = db.prepare("SELECT path FROM archive_driving_evidence")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for path in paths {
        if !candidates.contains(&path) {
            db.execute("DELETE FROM archive_driving_evidence WHERE path=?1", [path])?;
        }
    }
    tracing::info!(
        "RecentClips filter: scanned {scanned} camera files, reused {reused} cached results"
    );
    tracing::info!("RecentClips filter: camera verdicts: {non_park} non-Park (keep), {park_only} Park-only (skip), {no_gear} no SEI gear data (skip)");
    Ok(selected)
}

/// Write only after all candidates have been classified. The caller replaces
/// its candidate list only on success, so an I/O failure cannot allow unfiltered
/// RecentClips through to an archive backend.
pub fn filter_file(root: &Path, input: &Path, output: &Path, database: &Path) -> Result<()> {
    anyhow::ensure!(
        input != output,
        "archive filter input and output must differ"
    );
    let candidates = std::fs::read_to_string(input).context("Read archive candidates")?;
    let selected = filter_cached(root, &candidates, database, read_evidence)?;
    let skipped = candidates.lines().count() - selected.len();
    let mut result = selected.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    std::fs::write(output, result).context("Write filtered archive candidates")?;
    tracing::info!(
        "RecentClips filter complete: keeping {} candidates, skipping {skipped}",
        selected.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_shared_cache_is_rebuilt_without_touching_drive_data() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let no_sei = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let parked = "RecentClips/2026-10-24_13-00-00-front.mp4";
        std::fs::write(root.path().join(no_sei), b"\0\0\0\x08mdat").unwrap();
        std::fs::write(root.path().join(parked), mp4(0, 30.0)).unwrap();
        let database = root.path().join("drives.db");
        let db = crate::db::open_connection(&database).unwrap();
        crate::schema::migrate(&db).unwrap();
        db.execute_batch(
            "DROP TABLE archive_driving_evidence;
             CREATE TABLE archive_driving_evidence (
                path TEXT PRIMARY KEY, identity TEXT NOT NULL,
                gear INTEGER NOT NULL, max_speed REAL NOT NULL);
             INSERT INTO processed_files VALUES ('existing.mp4', 123);",
        )
        .unwrap();
        for (clip, speed) in [(no_sei, 0.0), (parked, 30.0)] {
            db.execute(
                "INSERT INTO archive_driving_evidence VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![
                    clip,
                    source_identity(&root.path().join(clip)).unwrap(),
                    speed
                ],
            )
            .unwrap();
        }
        drop(db);
        let candidates = format!("{no_sei}\n{parked}\n");
        let input = root.path().join("input");
        let output = root.path().join("output");
        std::fs::write(&input, &candidates).unwrap();
        filter_file(root.path(), &input, &output, &database).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "");
        assert_eq!(
            filter_cached(root.path(), &candidates, &database, |_| panic!(
                "cache should survive repeated migration"
            ))
            .unwrap(),
            Vec::<String>::new()
        );
        let db = crate::db::open_connection(&database).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT added_at FROM processed_files WHERE file='existing.mp4'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            123
        );
    }

    #[test]
    fn video_scan_does_not_hold_the_shared_database_writer_lock() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        std::fs::write(root.path().join(clip), mp4(1, 0.0)).unwrap();
        let database = root.path().join("drives.db");
        filter_cached(root.path(), clip, &database, |_| {
            let writer = crate::db::open_connection(&database)?;
            writer.busy_timeout(Duration::ZERO)?;
            writer.execute(
                "INSERT INTO processed_files VALUES ('concurrent.mp4', 123)",
                [],
            )?;
            Ok(DrivingEvidence {
                keep: true,
                no_gear: false,
            })
        })
        .unwrap();
        let db = crate::db::open_connection(&database).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM processed_files WHERE file='concurrent.mp4'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn persistent_cache_reuses_positive_and_negative_evidence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let driving = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let parked = "RecentClips/2026-10-24_13-00-00-front.mp4";
        std::fs::write(root.path().join(driving), mp4(1, 0.0)).unwrap();
        std::fs::write(root.path().join(parked), mp4(0, 0.0)).unwrap();
        let candidates = format!("{driving}\n{parked}\n");
        let cache = root.path().join("cache.sqlite");
        assert_eq!(
            filter_cached(root.path(), &candidates, &cache, read_evidence).unwrap(),
            [driving]
        );
        // Each call opens the on-disk database afresh, like a new process/boot.
        let no_reads =
            |_: &Path| -> Result<DrivingEvidence> { panic!("cached clip was rescanned") };
        assert_eq!(
            filter_cached(root.path(), &candidates, &cache, no_reads).unwrap(),
            [driving]
        );
        // Changing a file invalidates its evidence; the unchanged parked file
        // still hits the cache. Use size to avoid filesystem timestamp precision.
        std::fs::write(root.path().join(driving), b"changed").unwrap();
        let mut reads = 0;
        filter_cached(root.path(), &candidates, &cache, |_| {
            reads += 1;
            Ok(DrivingEvidence {
                keep: true,
                no_gear: false,
            })
        })
        .unwrap();
        assert_eq!(reads, 1);
        assert_eq!(
            filter_cached(root.path(), &candidates, &cache, no_reads).unwrap(),
            [driving]
        );
    }

    #[test]
    fn failed_reads_are_retried_and_obsolete_entries_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        std::fs::write(root.path().join(clip), mp4(1, 0.0)).unwrap();
        let cache = root.path().join("cache.sqlite");
        assert!(
            filter_cached(root.path(), clip, &cache, |_| anyhow::bail!("I/O failure"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            filter_cached(root.path(), clip, &cache, read_evidence).unwrap(),
            [clip]
        );
        filter_cached(root.path(), "", &cache, read_evidence).unwrap();
        let db = rusqlite::Connection::open(&cache).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM archive_driving_evidence", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn switching_snapshot_link_invalidates_cached_evidence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let a = root.path().join("snapshot-a");
        let b = root.path().join("snapshot-b");
        std::fs::write(&a, mp4(0, 0.0)).unwrap();
        std::fs::write(&b, mp4(1, 0.0)).unwrap();
        let link = root.path().join(clip);
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let cache = root.path().join("cache.sqlite");
        assert!(filter_cached(root.path(), clip, &cache, read_evidence)
            .unwrap()
            .is_empty());
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&b, &link).unwrap();
        assert_eq!(
            filter_cached(root.path(), clip, &cache, read_evidence).unwrap(),
            [clip]
        );
    }

    #[test]
    fn scans_recordings_in_order_but_preserves_candidate_output_order() {
        let later = "RecentClips/2026-10-24_13-00-00-front.mp4";
        let earlier = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let mut scanned = Vec::new();
        let selected = filter_with(&format!("{later}\n{earlier}\n"), |path| {
            scanned.push(path.to_owned());
            true
        });
        assert_eq!(scanned, [earlier, later]);
        assert_eq!(selected, [later, earlier]);
    }

    #[test]
    fn keeps_all_angles_of_driving_recordings_and_other_categories() {
        let prefix = "RecentClips/2026-10-24/2026-10-24_12-00-00";
        let front = format!("{prefix}-front.mp4");
        let rear = format!("{prefix}-back.mp4");
        let parked = "RecentClips/2026-10-24/2026-10-24_13-00-00-front.mp4";
        let saved = "SavedClips/event/front.mp4";
        let input = format!("{rear}\n{front}\n{parked}\n{saved}\nRecentClips/unrecognized.mp4\n");
        let mut scans = Vec::new();
        let selected = filter_with(&input, |path| {
            scans.push(path.to_owned());
            path == front
        });
        assert_eq!(selected, [rear, front.clone(), saved.to_owned()]);
        assert!(scans.contains(&front));
        assert!(!scans.iter().any(|path| path.ends_with("-back.mp4")));
    }

    #[test]
    fn only_front_is_checked_even_if_missing_from_the_candidate_list() {
        let front = "RecentClips/2026-10-24/2026-10-24_12-00-00-front.mp4";
        let left = "RecentClips/2026-10-24/2026-10-24_12-00-00-left_repeater.mp4";
        let mut scanned = Vec::new();
        assert_eq!(
            filter_with(left, |path| {
                scanned.push(path.to_owned());
                path == front
            }),
            [left]
        );
        assert_eq!(scanned, [front]);
        assert!(filter_with(&format!("{front}\n{left}\n"), |path| {
            assert_eq!(path, front);
            false
        })
        .is_empty());
    }

    // Synthetic length-prefixed H.264 SEI in a real MP4 mdat box, fed through
    // the production extractor rather than a mocked driving verdict.
    fn mp4(gear: u8, speed: f32) -> Vec<u8> {
        mp4_with_gps(gear, speed, Some((37.5, -122.25)))
    }

    fn mp4_with_gps(gear: u8, speed: f32, gps: Option<(f64, f64)>) -> Vec<u8> {
        // proto3 omits Park (the zero enum value), so exercise that wire form.
        let mut proto = vec![0x08, 0x01];
        if gear != 0 {
            proto.extend([0x10, gear]);
        }
        proto.push(0x25);
        proto.extend(speed.to_le_bytes());
        if let Some((lat, lon)) = gps {
            proto.push(0x59);
            proto.extend(lat.to_le_bytes());
            proto.push(0x61);
            proto.extend(lon.to_le_bytes());
        }
        let mut nal = vec![0x06, 0x05, 0x00, 0x42, 0x42, 0x42, 0x69];
        nal.extend(proto);
        nal.push(0x80);
        let mut result = ((12 + nal.len()) as u32).to_be_bytes().to_vec();
        result.extend(b"mdat");
        result.extend((nal.len() as u32).to_be_bytes());
        result.extend(nal);
        result
    }

    #[test]
    fn repeater_data_never_overrides_the_front_camera_verdict() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let front = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let repeater = "RecentClips/2026-10-24_12-00-00-right_repeater.mp4";
        let no_sei = b"\0\0\0\x0emdat\0\0\0\x02\x65\x80".to_vec();
        let database = root.path().join("drives.db");
        let candidates = format!("{front}\n{repeater}\n");
        // Park on front wins over both a metadata-free and a non-Park repeater.
        for rear_bytes in [no_sei.clone(), mp4(1, 0.0)] {
            std::fs::write(root.path().join(front), mp4(0, 30.0)).unwrap();
            std::fs::write(root.path().join(repeater), rear_bytes).unwrap();
            assert!(
                filter_cached(root.path(), &candidates, &database, read_evidence)
                    .unwrap()
                    .is_empty()
            );
            // The front can already have been archived; still inspect its file.
            assert!(
                filter_cached(root.path(), repeater, &database, read_evidence)
                    .unwrap()
                    .is_empty()
            );
        }
        // Non-Park front SEI keeps all associated cameras.
        std::fs::write(root.path().join(front), mp4(1, 0.0)).unwrap();
        assert_eq!(
            filter_cached(root.path(), &candidates, &database, read_evidence).unwrap(),
            [front, repeater]
        );
        assert_eq!(
            filter_cached(root.path(), repeater, &database, |_| panic!(
                "front verdict should remain cached"
            ))
            .unwrap(),
            [repeater]
        );
        // No front SEI must skip even when the repeater reports Drive.
        std::fs::write(root.path().join(front), no_sei).unwrap();
        assert!(
            filter_cached(root.path(), &candidates, &database, read_evidence)
                .unwrap()
                .is_empty()
        );
        // An unavailable front is retried, never replaced by a repeater verdict.
        std::fs::remove_file(root.path().join(front)).unwrap();
        assert!(
            filter_cached(root.path(), &candidates, &database, read_evidence)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn driving_only_filters_mixed_recordings_and_preserves_other_categories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let cameras = [
            "front",
            "back",
            "left_repeater",
            "right_repeater",
            "left_pillar",
            "right_pillar",
        ];
        let no_sei = b"\0\0\0\x0emdat\0\0\0\x02\x65\x80";
        let mut candidates = Vec::new();
        let mut expected = Vec::new();
        for (hour, gear) in [
            (10, Some(0)),
            (11, Some(1)),
            (12, Some(2)),
            (13, Some(3)),
            (14, None),
        ] {
            for camera in cameras {
                let path = format!("RecentClips/2026-11-06_{hour:02}-00-00-{camera}.mp4");
                // Only front carries SEI. Park has noisy speed; D/R/N are stopped.
                let bytes = match (camera, gear) {
                    ("front", Some(gear)) => {
                        mp4_with_gps(gear, if gear == 0 { 30.0 } else { 0.0 }, None)
                    }
                    _ => no_sei.to_vec(),
                };
                std::fs::write(root.path().join(&path), bytes).unwrap();
                if matches!(gear, Some(1..=3)) {
                    expected.push(path.clone());
                }
                candidates.push(path);
            }
        }
        // No SEI lookup should be attempted for unrelated archive categories.
        for path in [
            "SavedClips/event/front.mp4",
            "SentryClips/event/front.mp4",
            "TeslaTrackMode/event/front.mp4",
        ] {
            candidates.push(path.to_owned());
            expected.push(path.to_owned());
        }
        let input = root.path().join("input");
        let output = root.path().join("output");
        let database = root.path().join("drives.db");
        let candidates = format!("{}\n", candidates.join("\n"));
        std::fs::write(&input, &candidates).unwrap();
        filter_file(root.path(), &input, &output, &database).unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("{}\n", expected.join("\n"))
        );
        assert_eq!(
            filter_cached(root.path(), &candidates, &database, |_| panic!(
                "all front verdicts should already be cached"
            ))
            .unwrap(),
            expected
        );
    }

    #[test]
    fn gear_only_contract_keeps_all_camera_angles_with_fresh_and_cached_verdicts() {
        for gear in [Some(0), Some(1), Some(2), Some(3), None] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("RecentClips")).unwrap();
            let clips: Vec<_> = ["front", "back", "left-repeater", "right-repeater"]
                .iter()
                .map(|camera| format!("RecentClips/2026-10-24_12-00-00-{camera}.mp4"))
                .collect();
            for clip in &clips {
                let bytes = match gear {
                    Some(gear) => mp4_with_gps(gear, 0.0, None),
                    None => b"\0\0\0\x0emdat\0\0\0\x02\x65\x80".to_vec(),
                };
                std::fs::write(root.path().join(clip), bytes).unwrap();
            }
            let input = root.path().join("input");
            let output = root.path().join("output");
            let database = root.path().join("drives.db");
            let candidates = format!("{}\n", clips.join("\n"));
            std::fs::write(&input, &candidates).unwrap();
            filter_file(root.path(), &input, &output, &database).unwrap();
            let expected = if gear.is_none() || gear == Some(0) {
                ""
            } else {
                &candidates
            };
            assert_eq!(
                std::fs::read_to_string(&output).unwrap(),
                expected,
                "gear={gear:?}"
            );
            assert_eq!(
                filter_cached(root.path(), &candidates, &database, |_| panic!(
                    "unchanged recordings must use cached gear verdicts"
                ))
                .unwrap()
                .join("\n"),
                expected.trim_end(),
                "cached gear={gear:?}"
            );
        }
    }

    #[test]
    fn non_park_sample_anywhere_in_front_keeps_the_whole_recording() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let front = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let back = "RecentClips/2026-10-24_12-00-00-back.mp4";
        std::fs::write(root.path().join(back), mp4_with_gps(0, 0.0, None)).unwrap();
        // Park -> stopped in Drive -> Park, with no coordinates throughout.
        let mut body = Vec::new();
        for gear in [0, 1, 0] {
            body.extend_from_slice(&mp4_with_gps(gear, 0.0, None)[8..]);
        }
        let mut bytes = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        bytes.extend(b"mdat");
        bytes.extend(body);
        std::fs::write(root.path().join(front), bytes).unwrap();
        assert_eq!(
            filter_cached(
                root.path(),
                &format!("{front}\n{back}\n"),
                &root.path().join("drives.db"),
                read_evidence
            )
            .unwrap(),
            [front, back]
        );
    }

    #[test]
    fn archive_gear_is_independent_of_missing_or_invalid_gps() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        for gps in [
            None,
            Some((0.0, 0.0)),
            Some((f64::NAN, 0.0)),
            Some((91.0, 0.0)),
        ] {
            for gear in [0, 1, 2, 3] {
                std::fs::write(root.path().join(clip), mp4_with_gps(gear, 30.0, gps)).unwrap();
                // Mapping still rejects coordinates which cannot be plotted.
                assert!(crate::extract::extract_gps_from_file(
                    &root.path().join(clip).to_string_lossy()
                )
                .unwrap()
                .gear_states
                .is_empty());
                let evidence = read_evidence(&root.path().join(clip)).unwrap();
                assert!(!evidence.no_gear, "decoded gear must survive GPS rejection");
                assert_eq!(evidence.keep, gear != 0, "gear={gear}, GPS={gps:?}");
                assert_eq!(
                    !filter_cached(
                        root.path(),
                        clip,
                        &root.path().join("drives.db"),
                        read_evidence
                    )
                    .unwrap()
                    .is_empty(),
                    gear != 0
                );
            }
        }
    }

    #[test]
    fn gps_gated_cache_verdicts_are_discarded_and_recomputed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let parked = "RecentClips/2026-10-24_12-00-00-front.mp4";
        let no_sei = "RecentClips/2026-10-24_13-00-00-front.mp4";
        std::fs::write(root.path().join(parked), mp4_with_gps(0, 0.0, None)).unwrap();
        std::fs::write(
            root.path().join(no_sei),
            b"\0\0\0\x0emdat\0\0\0\x02\x65\x80",
        )
        .unwrap();
        let database = root.path().join("drives.db");
        let db = crate::db::open_connection(&database).unwrap();
        crate::schema::migrate(&db).unwrap();
        db.execute_batch("DROP TABLE archive_driving_evidence;
            CREATE TABLE archive_driving_evidence (path TEXT PRIMARY KEY, identity TEXT NOT NULL, keep INTEGER NOT NULL);").unwrap();
        for clip in [parked, no_sei] {
            db.execute(
                "INSERT INTO archive_driving_evidence VALUES (?1, ?2, 1)",
                rusqlite::params![clip, source_identity(&root.path().join(clip)).unwrap()],
            )
            .unwrap();
        }
        drop(db);
        let candidates = format!("{parked}\n{no_sei}\n");
        assert_eq!(
            filter_cached(root.path(), &candidates, &database, read_evidence).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            filter_cached(root.path(), &candidates, &database, |_| panic!(
                "fresh verdict should be cached"
            ))
            .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn embedded_gear_alone_controls_selection() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        for (gear, speed, expected) in [
            (0, 0.0, false),
            (1, 0.0, true),
            (2, 0.2, true),
            (0, 0.5, false),
            (0, 0.6, false),
            (0, 30.0, false),
            (3, 0.0, true),
        ] {
            let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
            std::fs::write(root.path().join(clip), mp4(gear, speed)).unwrap();
            assert_eq!(
                !filter_driving_recent(root.path(), clip).is_empty(),
                expected,
                "gear={gear}, speed={speed}"
            );
        }
    }

    #[test]
    fn old_cached_no_sei_keep_verdict_is_skipped_without_rescanning() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        std::fs::write(root.path().join(clip), b"\0\0\0\x08mdat").unwrap();
        let database = root.path().join("drives.db");
        let db = crate::db::open_connection(&database).unwrap();
        crate::schema::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO archive_driving_evidence (path, identity, keep, no_gear) VALUES (?1, ?2, 1, 1)",
            rusqlite::params![clip, source_identity(&root.path().join(clip)).unwrap()],
        ).unwrap();
        drop(db);
        assert!(filter_cached(root.path(), clip, &database, |_| panic!(
            "policy change must reuse cached no-SEI evidence"
        ))
        .unwrap()
        .is_empty());
    }

    #[test]
    fn missing_clips_are_retried_and_metadata_free_clips_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        let clip = "RecentClips/2026-10-24_12-00-00-front.mp4";
        assert!(filter_driving_recent(root.path(), clip).is_empty());
        std::fs::create_dir(root.path().join("RecentClips")).unwrap();
        std::fs::write(root.path().join(clip), b"\0\0\0\x08mdat").unwrap();
        assert!(filter_driving_recent(root.path(), clip).is_empty());
        let database = root.path().join("drives.db");
        assert_eq!(
            filter_cached(root.path(), clip, &database, read_evidence).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            filter_cached(root.path(), clip, &database, |_| panic!(
                "no-SEI verdict should be cached"
            ))
            .unwrap(),
            Vec::<String>::new()
        );
    }
}

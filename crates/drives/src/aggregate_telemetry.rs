//! BLE telemetry → per-route aggregates.
//!
//! Telemetry samples written by `sentryusb-tesla-telemetry` live in
//! `telemetry_samples`, keyed by unix-second `ts`. The drives page
//! wants per-clip rollups (battery delta, min/max cabin temp, HVAC
//! runtime) without paying for a join on every render — so when we
//! insert (or upsert) a route, we run this module to compute the
//! v6 columns and write them onto the route row.
//!
//! Window derivation: Tesla clip filenames embed the wall-clock
//! start of the clip (`YYYY-MM-DD_HH-MM-SS`), and the recorder
//! splits at 1-minute boundaries (matching `clip_duration_ms` in
//! `aggregate.rs`). So the per-clip window is
//! `[clip_ts, clip_ts + 60s]`.

use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};

use crate::grouper::parse_clip_timestamp;
pub use crate::types::RouteTelemetryAggregates;

/// Clip duration in seconds — must match `clip_duration_ms` in
/// `aggregate.rs` (60 s).
const CLIP_WINDOW_SECS: i64 = 60;

/// Derive the unix-second window for a route from its `file` path.
/// Returns `None` when the filename doesn't embed a parseable
/// timestamp — pre-grouper / corrupted paths just skip telemetry.
pub fn window_for_route_file(file: &str) -> Option<(i64, i64)> {
    // Basename parse: gap-fill routes live at event-folder paths whose
    // folder timestamp a left-to-right scan would pick over the clip's own,
    // matching telemetry against the wrong minute.
    let naive = parse_clip_timestamp(file)?;
    // Tesla writes the filename using the car's local clock, and the
    // Pi writes telemetry samples using its own local clock — on the
    // same install these are the same zone, so reading both as
    // `Local` keeps the comparison consistent. `earliest()` rather
    // than `single()`: during the DST fall-back hour local times are
    // ambiguous and `single()` returned None, silently skipping
    // telemetry for every clip in that hour once a year.
    let local = Local.from_local_datetime(&naive).earliest()?;
    let start = local.timestamp();
    Some((start, start + CLIP_WINDOW_SECS))
}

/// Compute the per-clip rollup over `telemetry_samples` for the given
/// `[start_ts, end_ts]` window (inclusive). All fields are None when
/// no samples landed in the window — the caller writes NULLs in that
/// case and the routes row stays as if telemetry never ran.
pub fn compute_telemetry_for_window(
    conn: &Connection,
    start_ts: i64,
    end_ts: i64,
) -> Result<RouteTelemetryAggregates> {
    // One scan, aggregate everything in SQL. `MIN(ts)` and `MAX(ts)`
    // pick the rows whose battery_pct values are the "start" and "end"
    // of the window without a second pass.
    let row = conn
        .query_row(
            "SELECT
                count(*),
                count(battery_pct),
                count(hvac_on),
                sum(CASE WHEN hvac_on = 1 THEN 1 ELSE 0 END),
                min(interior_temp_c),
                max(interior_temp_c),
                avg(exterior_temp_c)
             FROM telemetry_samples
             WHERE ts BETWEEN ?1 AND ?2",
            params![start_ts, end_ts],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,              // total
                    r.get::<_, i64>(1)?,              // battery_pct count
                    r.get::<_, i64>(2)?,              // hvac_on count
                    r.get::<_, Option<i64>>(3)?,      // hvac_on=1 count
                    r.get::<_, Option<f64>>(4)?,      // interior min
                    r.get::<_, Option<f64>>(5)?,      // interior max
                    r.get::<_, Option<f64>>(6)?,      // exterior avg
                ))
            },
        )
        .optional()?
        .unwrap_or((0, 0, 0, None, None, None, None));

    let (
        total_samples,
        _bat_count,
        hvac_total,
        hvac_on_count,
        interior_temp_min,
        interior_temp_max,
        exterior_temp_avg,
    ) = row;

    if total_samples == 0 {
        return Ok(RouteTelemetryAggregates::default());
    }

    // Battery percent start/end: separate point queries, ordered by
    // ts. Cheaper than dragging full battery_pct timeseries through
    // the avg-aware aggregate.
    let battery_pct_start: Option<f64> = conn
        .query_row(
            "SELECT battery_pct FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND battery_pct IS NOT NULL \
             ORDER BY ts ASC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;
    let battery_pct_end: Option<f64> = conn
        .query_row(
            "SELECT battery_pct FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND battery_pct IS NOT NULL \
             ORDER BY ts DESC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;

    // TPMS — latest non-null reading per tire within the window.
    // Pressures change slowly so the most recent sample is the
    // canonical "what was it during this clip" value.
    let latest_tire = |col: &str| -> Result<Option<f64>> {
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {col} FROM telemetry_samples \
                     WHERE ts BETWEEN ?1 AND ?2 AND {col} IS NOT NULL \
                     ORDER BY ts DESC LIMIT 1"
                ),
                params![start_ts, end_ts],
                |r| r.get(0),
            )
            .optional()?)
    };
    let tire_fl_psi = latest_tire("tire_fl_psi")?;
    let tire_fr_psi = latest_tire("tire_fr_psi")?;
    let tire_rl_psi = latest_tire("tire_rl_psi")?;
    let tire_rr_psi = latest_tire("tire_rr_psi")?;

    // Odometer: first / last non-null in the window. Together they
    // give a per-clip mileage delta, which the per-drive rollup
    // then chains across all clips for the trip's actual distance.
    let odometer_mi_start: Option<f64> = conn
        .query_row(
            "SELECT odometer_mi FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND odometer_mi IS NOT NULL \
             ORDER BY ts ASC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;
    let odometer_mi_end: Option<f64> = conn
        .query_row(
            "SELECT odometer_mi FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND odometer_mi IS NOT NULL \
             ORDER BY ts DESC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;

    // software_version intentionally not fetched — Tesla doesn't
    // expose car_version over BLE, so this column is always NULL.

    // v10 location_name: first / last non-null reverse-geocoded
    // address in the clip window. Drive-level rollup chains these
    // and (optionally) overrides with "Home"/"Work" based on the
    // user's saved coords in `meta`.
    let location_name_start: Option<String> = conn
        .query_row(
            "SELECT location_name FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND location_name IS NOT NULL \
             ORDER BY ts ASC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;
    let location_name_end: Option<String> = conn
        .query_row(
            "SELECT location_name FROM telemetry_samples \
             WHERE ts BETWEEN ?1 AND ?2 AND location_name IS NOT NULL \
             ORDER BY ts DESC LIMIT 1",
            params![start_ts, end_ts],
            |r| r.get(0),
        )
        .optional()?;

    // hvac_runtime only meaningful when at least one sample populated
    // the hvac_on column. If every sample's hvac_on is NULL (the
    // body-controller-state sampler path doesn't fill it), leave the
    // runtime as None rather than emitting a fake zero.
    let hvac_runtime_s = if hvac_total == 0 {
        None
    } else {
        let on = hvac_on_count.unwrap_or(0);
        let window = (end_ts - start_ts).max(1) as f64;
        // Each non-null hvac sample represents `window / hvac_total`
        // seconds of the window — multiplying by the on-count gives
        // the estimated runtime. Clamp to the window to defend
        // against duplicates or clock jitter widening the count.
        let est = (on as f64) * (window / hvac_total as f64);
        Some(est.round().clamp(0.0, window) as i64)
    };

    Ok(RouteTelemetryAggregates {
        battery_pct_start,
        battery_pct_end,
        interior_temp_min,
        interior_temp_max,
        exterior_temp_avg,
        hvac_runtime_s,
        tire_fl_psi,
        tire_fr_psi,
        tire_rl_psi,
        tire_rr_psi,
        odometer_mi_start,
        odometer_mi_end,
        location_name_start,
        location_name_end,
    })
}

/// Compute the rollup for a route by its file path. Convenience
/// wrapper that does the window derivation in one step. Returns
/// `Ok(default)` when the filename doesn't carry a parseable
/// timestamp (event-folder paths, manual imports).
pub fn compute_telemetry_for_route(
    conn: &Connection,
    file: &str,
) -> Result<RouteTelemetryAggregates> {
    match window_for_route_file(file) {
        Some((start, end)) => compute_telemetry_for_window(conn, start, end),
        None => Ok(RouteTelemetryAggregates::default()),
    }
}

/// Write the rollup onto the routes row. Idempotent — re-running
/// with a fresh rollup just overwrites the previous values, which
/// is what we want as more samples arrive for the same window.
pub fn write_route_telemetry(
    conn: &Connection,
    file: &str,
    agg: &RouteTelemetryAggregates,
) -> Result<()> {
    // `battery_temp_avg` is left out of the UPDATE entirely — we don't
    // populate it any more. The column stays in the schema as nullable
    // forward-compat; old rows that happened to have a value (none in
    // practice, since the sampler never wrote one) are preserved.
    conn.execute(
        "UPDATE routes SET
            battery_pct_start = ?1,
            battery_pct_end   = ?2,
            interior_temp_min = ?3,
            interior_temp_max = ?4,
            exterior_temp_avg = ?5,
            hvac_runtime_s    = ?6,
            tire_fl_psi       = ?7,
            tire_fr_psi       = ?8,
            tire_rl_psi       = ?9,
            tire_rr_psi          = ?10,
            odometer_mi_start    = ?11,
            odometer_mi_end      = ?12,
            location_name_start  = ?13,
            location_name_end    = ?14
         WHERE file = ?15",
        params![
            agg.battery_pct_start,
            agg.battery_pct_end,
            agg.interior_temp_min,
            agg.interior_temp_max,
            agg.exterior_temp_avg,
            agg.hvac_runtime_s,
            agg.tire_fl_psi,
            agg.tire_fr_psi,
            agg.tire_rl_psi,
            agg.tire_rr_psi,
            agg.odometer_mi_start,
            agg.odometer_mi_end,
            agg.location_name_start,
            agg.location_name_end,
            file,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=MEMORY;").unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn
    }

    fn seed(conn: &Connection, ts: i64, bat: Option<f64>, int_t: Option<f64>, ext_t: Option<f64>, bt_t: Option<f64>, hvac: Option<bool>) {
        conn.execute(
            "INSERT INTO telemetry_samples (ts, battery_pct, battery_temp_c, interior_temp_c, exterior_temp_c, hvac_on, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'state')",
            params![ts, bat, bt_t, int_t, ext_t, hvac.map(|b| if b { 1_i64 } else { 0_i64 })],
        ).unwrap();
    }

    #[test]
    fn empty_window_returns_default() {
        let conn = fresh_db();
        let a = compute_telemetry_for_window(&conn, 1_700_000_000, 1_700_000_060).unwrap();
        assert_eq!(a, RouteTelemetryAggregates::default());
    }

    #[test]
    fn battery_start_end_from_ordered_samples() {
        let conn = fresh_db();
        seed(&conn, 1_700_000_000, Some(73.0), None, None, None, None);
        seed(&conn, 1_700_000_030, Some(72.5), None, None, None, None);
        seed(&conn, 1_700_000_060, Some(72.0), None, None, None, None);
        let a = compute_telemetry_for_window(&conn, 1_700_000_000, 1_700_000_060).unwrap();
        assert_eq!(a.battery_pct_start, Some(73.0));
        assert_eq!(a.battery_pct_end, Some(72.0));
    }

    #[test]
    fn temperatures_aggregate_correctly() {
        let conn = fresh_db();
        seed(&conn, 1_700_000_000, None, Some(20.0), Some(10.0), Some(15.0), None);
        seed(&conn, 1_700_000_030, None, Some(22.0), Some(11.0), Some(16.0), None);
        seed(&conn, 1_700_000_060, None, Some(24.0), Some(9.0), Some(17.0), None);
        let a = compute_telemetry_for_window(&conn, 1_700_000_000, 1_700_000_060).unwrap();
        assert_eq!(a.interior_temp_min, Some(20.0));
        assert_eq!(a.interior_temp_max, Some(24.0));
        assert_eq!(a.exterior_temp_avg, Some(10.0));
    }

    #[test]
    fn hvac_runtime_proportional_to_on_count() {
        let conn = fresh_db();
        // 4 samples in a 60s window, 2 with HVAC on. Estimated
        // runtime: 60 / 4 * 2 = 30s.
        seed(&conn, 1_700_000_000, Some(50.0), None, None, None, Some(false));
        seed(&conn, 1_700_000_015, Some(50.0), None, None, None, Some(true));
        seed(&conn, 1_700_000_030, Some(50.0), None, None, None, Some(true));
        seed(&conn, 1_700_000_045, Some(50.0), None, None, None, Some(false));
        let a = compute_telemetry_for_window(&conn, 1_700_000_000, 1_700_000_060).unwrap();
        assert_eq!(a.hvac_runtime_s, Some(30));
    }

    #[test]
    fn hvac_runtime_none_when_no_hvac_samples() {
        let conn = fresh_db();
        // Samples exist but none have hvac_on populated → hvac
        // runtime stays None.
        seed(&conn, 1_700_000_000, Some(50.0), None, None, None, None);
        let a = compute_telemetry_for_window(&conn, 1_700_000_000, 1_700_000_060).unwrap();
        assert!(a.hvac_runtime_s.is_none());
    }

    #[test]
    fn window_for_route_file_parses_recent_clips() {
        let (start, end) = window_for_route_file(
            "RecentClips/2026-05-17/2026-05-17_18-47-34-front.mp4",
        )
        .expect("should parse");
        assert_eq!(end - start, CLIP_WINDOW_SECS);
    }

    #[test]
    fn window_for_route_file_none_for_unparseable() {
        assert!(window_for_route_file("RecentClips/garbage.mp4").is_none());
    }

    #[test]
    fn write_route_telemetry_persists_rollup() {
        let conn = fresh_db();
        // Need a routes row to UPDATE.
        conn.execute(
            "INSERT INTO routes (file, date_dir, points_blob, updated_at) VALUES (?1, ?2, X'', 0)",
            params!["RecentClips/2026-05-17/2026-05-17_18-47-34-front.mp4", "RecentClips"],
        )
        .unwrap();
        let agg = RouteTelemetryAggregates {
            battery_pct_start: Some(73.0),
            battery_pct_end: Some(72.0),
            interior_temp_min: Some(20.0),
            interior_temp_max: Some(24.0),
            exterior_temp_avg: Some(10.5),
            hvac_runtime_s: Some(45),
            ..Default::default()
        };
        write_route_telemetry(
            &conn,
            "RecentClips/2026-05-17/2026-05-17_18-47-34-front.mp4",
            &agg,
        )
        .unwrap();
        #[allow(clippy::type_complexity)] // row tuple mirrors the SELECT columns
        let (bs, be, imin, imax, ea, hvac): (
            Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<i64>,
        ) = conn.query_row(
            "SELECT battery_pct_start, battery_pct_end, \
                    interior_temp_min, interior_temp_max, exterior_temp_avg, hvac_runtime_s \
             FROM routes WHERE file = ?1",
            params!["RecentClips/2026-05-17/2026-05-17_18-47-34-front.mp4"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        ).unwrap();
        assert_eq!(bs, Some(73.0));
        assert_eq!(be, Some(72.0));
        assert_eq!(imin, Some(20.0));
        assert_eq!(imax, Some(24.0));
        assert_eq!(ea, Some(10.5));
        assert_eq!(hvac, Some(45));
    }
}

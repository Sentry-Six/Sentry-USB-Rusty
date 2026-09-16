//! Telemetry access to the shared drives SQLite database.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::sample::Sample;

/// Opens the canonical database and applies idempotent drives migrations.
pub fn open() -> Result<Connection> {
    let path = sentryusb_drives::DEFAULT_DB_PATH;
    // SQLite cannot create a missing parent directory.
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open {}", path))?;
    // The timeout covers transient write contention with the main server.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )?;
    sentryusb_drives::schema::migrate(&conn)
        .context("schema migrate failed in telemetry sampler")?;
    Ok(conn)
}

/// Inserts a sample. On a `ts` collision (the PK) the incoming non-null fields
/// merge into the existing row (a same-second refresh keeps its battery/charge),
/// and `source` stays 'state' once set so the source='state' reads keep finding it.
pub fn insert(conn: &Connection, s: &Sample) -> Result<()> {
    // BLE does not expose `car_version`; `software_version` remains nullable.
    conn.execute(
        "INSERT INTO telemetry_samples \
         (ts, battery_pct, battery_temp_c, interior_temp_c, exterior_temp_c, hvac_on, \
          tire_fl_psi, tire_fr_psi, tire_rl_psi, tire_rr_psi, \
          odometer_mi, location_name, \
          charger_power_kw, charger_actual_current_a, charger_voltage_v, \
          charge_rate_mph, charge_energy_added_kwh, charge_limit_soc, battery_range_mi, \
          charging_amps_set, charge_current_request_max, charge_port_door_open, \
          latitude, longitude, \
          source, charge_minutes_to_full, charging_state) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
                 ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, \
                 ?25, ?26, ?27) \
         ON CONFLICT(ts) DO UPDATE SET \
           battery_pct                = COALESCE(excluded.battery_pct, battery_pct), \
           battery_temp_c             = COALESCE(excluded.battery_temp_c, battery_temp_c), \
           interior_temp_c            = COALESCE(excluded.interior_temp_c, interior_temp_c), \
           exterior_temp_c            = COALESCE(excluded.exterior_temp_c, exterior_temp_c), \
           hvac_on                    = COALESCE(excluded.hvac_on, hvac_on), \
           tire_fl_psi                = COALESCE(excluded.tire_fl_psi, tire_fl_psi), \
           tire_fr_psi                = COALESCE(excluded.tire_fr_psi, tire_fr_psi), \
           tire_rl_psi                = COALESCE(excluded.tire_rl_psi, tire_rl_psi), \
           tire_rr_psi                = COALESCE(excluded.tire_rr_psi, tire_rr_psi), \
           odometer_mi                = COALESCE(excluded.odometer_mi, odometer_mi), \
           location_name              = COALESCE(excluded.location_name, location_name), \
           charger_power_kw           = COALESCE(excluded.charger_power_kw, charger_power_kw), \
           charger_actual_current_a   = COALESCE(excluded.charger_actual_current_a, charger_actual_current_a), \
           charger_voltage_v          = COALESCE(excluded.charger_voltage_v, charger_voltage_v), \
           charge_rate_mph            = COALESCE(excluded.charge_rate_mph, charge_rate_mph), \
           charge_energy_added_kwh    = COALESCE(excluded.charge_energy_added_kwh, charge_energy_added_kwh), \
           charge_limit_soc           = COALESCE(excluded.charge_limit_soc, charge_limit_soc), \
           battery_range_mi           = COALESCE(excluded.battery_range_mi, battery_range_mi), \
           charging_amps_set          = COALESCE(excluded.charging_amps_set, charging_amps_set), \
           charge_current_request_max = COALESCE(excluded.charge_current_request_max, charge_current_request_max), \
           charge_port_door_open      = COALESCE(excluded.charge_port_door_open, charge_port_door_open), \
           latitude                   = COALESCE(excluded.latitude, latitude), \
           longitude                  = COALESCE(excluded.longitude, longitude), \
           source                     = CASE WHEN excluded.source = 'state' OR source = 'state' \
                                             THEN 'state' ELSE excluded.source END, \
           charge_minutes_to_full     = COALESCE(excluded.charge_minutes_to_full, charge_minutes_to_full), \
           charging_state             = COALESCE(excluded.charging_state, charging_state)",
        params![
            s.ts,
            s.battery_pct,
            s.battery_temp_c,
            s.interior_temp_c,
            s.exterior_temp_c,
            s.hvac_on.map(|b| if b { 1_i64 } else { 0_i64 }),
            s.tire_fl_psi,
            s.tire_fr_psi,
            s.tire_rl_psi,
            s.tire_rr_psi,
            s.odometer_mi,
            s.location_name,
            s.charger_power_kw,
            s.charger_actual_current_a,
            s.charger_voltage_v,
            s.charge_rate_mph,
            s.charge_energy_added_kwh,
            s.charge_limit_soc,
            s.battery_range_mi,
            s.charging_amps_set,
            s.charge_current_request_max,
            s.charge_port_door_open.map(|b| if b { 1_i64 } else { 0_i64 }),
            s.latitude,
            s.longitude,
            s.source,
            s.charge_minutes_to_full,
            s.charging_state,
        ],
    )?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=MEMORY;").unwrap();
        sentryusb_drives::schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_full_sample() {
        let conn = fresh_memory_db();
        let s = Sample {
            ts: 1_700_000_000,
            battery_pct: Some(73.0),
            battery_temp_c: Some(18.5),
            interior_temp_c: Some(22.0),
            exterior_temp_c: Some(12.0),
            hvac_on: Some(true),
            tire_fl_psi: Some(40.0),
            tire_fr_psi: Some(40.5),
            tire_rl_psi: Some(38.5),
            tire_rr_psi: Some(39.0),
            odometer_mi: Some(12453.5),
            location_name: Some("123 Main St".into()),
            source: "state".into(),
            ..Sample::default()
        };
        insert(&conn, &s).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM telemetry_samples", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_persists_charging_control_telemetry() {
        let conn = fresh_memory_db();
        let s = Sample {
            ts: 1_700_000_050,
            charging_amps_set: Some(32),
            charge_current_request_max: Some(48),
            charge_port_door_open: Some(true),
            source: "state".into(),
            ..Sample::default()
        };

        insert(&conn, &s).unwrap();
        let values: (Option<i64>, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT charging_amps_set, charge_current_request_max, charge_port_door_open \
                 FROM telemetry_samples WHERE ts = ?1",
                [s.ts],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(values, (Some(32), Some(48), Some(1)));
    }

    #[test]
    fn insert_sparse_body_controller_sample() {
        let conn = fresh_memory_db();
        let s = Sample {
            ts: 1_700_000_100,
            source: "body_controller".into(),
            ..Sample::default()
        };
        insert(&conn, &s).unwrap();
        let (pct, src): (Option<f64>, String) = conn
            .query_row(
                "SELECT battery_pct, source FROM telemetry_samples",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(pct.is_none());
        assert_eq!(src, "body_controller");
    }

    #[test]
    fn duplicate_ts_keeps_one_row() {
        let conn = fresh_memory_db();
        let s = Sample {
            ts: 1_700_000_200,
            source: "state".into(),
            ..Sample::default()
        };
        insert(&conn, &s).unwrap();
        insert(&conn, &s).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM telemetry_samples", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "a ts collision must not create a second row");
    }

    // Same-second body-controller row then refresh row must merge: battery kept,
    // existing fields not blanked, row stays source='state' for the ble.rs read.
    #[test]
    fn parked_awake_refresh_merges_into_bc_row() {
        let conn = fresh_memory_db();
        let ts = 1_789_000_000;
        // 1) body-controller poll persists first: no battery, carries a location.
        insert(
            &conn,
            &Sample {
                ts,
                source: "body_controller".into(),
                location_name: Some("123 Main St".into()),
                ..Sample::default()
            },
        )
        .unwrap();
        // 2) parked-awake refresh, same second, carries the battery.
        insert(
            &conn,
            &Sample {
                ts,
                source: "state".into(),
                battery_pct: Some(46.0),
                ..Sample::default()
            },
        )
        .unwrap();

        let (count, batt, loc, src): (i64, Option<f64>, Option<String>, String) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM telemetry_samples), \
                 battery_pct, location_name, source \
                 FROM telemetry_samples WHERE ts = ?1",
                [ts],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "collision must merge into one row, not two");
        assert_eq!(batt, Some(46.0), "battery from the refresh must be kept");
        assert_eq!(
            loc.as_deref(),
            Some("123 Main St"),
            "body-controller field must not be blanked by the merge"
        );
        assert_eq!(src, "state", "row must read as source='state' for the battery query");

        // The exact shape api/ble.rs uses to read the latest battery.
        let latest: f64 = conn
            .query_row(
                "SELECT battery_pct FROM telemetry_samples \
                 WHERE source = 'state' AND battery_pct IS NOT NULL ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(latest, 46.0, "source='state' battery read must now find the value");
    }

    // A 'state' row already present, then a same-second write from another source,
    // must not downgrade source away from 'state' or the source='state' battery read
    // would lose the latest charge row.
    #[test]
    fn state_row_keeps_provenance_on_reverse_collision() {
        let conn = fresh_memory_db();
        let ts = 1_789_000_100;
        insert(
            &conn,
            &Sample { ts, source: "state".into(), battery_pct: Some(42.0), ..Sample::default() },
        )
        .unwrap();
        insert(
            &conn,
            &Sample {
                ts,
                source: "body_controller".into(),
                location_name: Some("Home".into()),
                ..Sample::default()
            },
        )
        .unwrap();

        let (src, batt, loc): (String, Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT source, battery_pct, location_name FROM telemetry_samples WHERE ts = ?1",
                [ts],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(src, "state", "must not downgrade a state row to body_controller");
        assert_eq!(batt, Some(42.0), "battery must survive the reverse-order collision");
        assert_eq!(loc.as_deref(), Some("Home"), "the later field must still merge in");

        let latest: f64 = conn
            .query_row(
                "SELECT battery_pct FROM telemetry_samples \
                 WHERE source = 'state' AND battery_pct IS NOT NULL ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(latest, 42.0, "source='state' read must still find the battery");
    }
}

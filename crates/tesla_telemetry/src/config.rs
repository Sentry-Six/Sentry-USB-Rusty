//! Loads telemetry settings on each main-loop iteration so changes do not
//! require a daemon restart.

use anyhow::Result;

/// Default adapter when `BLE_ADAPTER` is unset.
pub const DEFAULT_ADAPTER: &str = "hci0";

/// Default home-geofence radius in meters.
pub const DEFAULT_HOME_RADIUS_M: f64 = 120.0;

/// Default post-charge release grace in minutes.
pub const DEFAULT_CHARGE_GRACE_MIN: u64 = 45;

/// Keep-accessory automation; requires explicit enablement and a home center.
#[derive(Debug, Clone, Default)]
pub struct KeepAccessoryConfig {
    pub enabled: bool,
    pub home_lat: Option<f64>,
    pub home_lon: Option<f64>,
    pub home_radius_m: f64,
    /// Opt-in: hold 12V through a home charge instead of releasing after the archive.
    pub hold_for_charge: bool,
    /// Minutes to keep 12V on after a charge completes before releasing.
    pub charge_grace_min: u64,
}

/// Snapshot of the BLE-relevant config values.
#[derive(Debug, Clone)]
pub struct BleConfig {
    pub enabled: bool,
    pub vin: String,
    /// Bluetooth device ID (`hci0`, `hci1`, ...).
    pub adapter: String,
    /// Keep-Accessory-Power automation (12V-powered Pis only).
    pub keep_accessory: KeepAccessoryConfig,
    /// Keeps GPS polling active for automatic Away Mode evaluation.
    pub away_auto_enabled: bool,
    /// Master opt-in for experimental telemetry fields.
    pub experimental: bool,
    /// Seconds between keep-awake `charge-port-close` nudges.
    pub keep_awake_interval_secs: u64,
    /// Master switch (`BLE_KEEP_AWAKE_ENABLED`); when false the sampler skips
    /// the CPC nudge. Pin/archive behavior is unchanged. Missing key => false.
    pub keep_awake_enabled: bool,
}

/// Default keep-awake nudge interval in seconds.
pub const DEFAULT_KEEP_AWAKE_INTERVAL_SECS: u64 = 60;

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            vin: String::new(),
            adapter: DEFAULT_ADAPTER.to_string(),
            keep_accessory: KeepAccessoryConfig::default(),
            away_auto_enabled: false,
            experimental: false,
            keep_awake_interval_secs: DEFAULT_KEEP_AWAKE_INTERVAL_SECS,
            keep_awake_enabled: false,
        }
    }
}

impl BleConfig {
    /// Reads the current configuration. BLE is disabled unless explicitly enabled.
    pub fn load() -> Result<Self> {
        Self::load_from(sentryusb_config::find_config_path())
    }

    fn load_from(config_path: &str) -> Result<Self> {
        let (active, commented) = sentryusb_config::parse_file(config_path)?;

        // BLE telemetry requires explicit enablement.
        let enabled =
            match sentryusb_config::get_config_value(&active, &commented, "BLE_ENABLED") {
                Some(v) => matches!(v.as_str(), "yes" | "true" | "1"),
                None => false,
            };

        let vin = active
            .get("TESLA_BLE_VIN")
            .cloned()
            .unwrap_or_default()
            .to_uppercase();

        // Accept hci-prefixed adapters that currently exist; otherwise use hci0.
        let configured = active
            .get("BLE_ADAPTER")
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("hci"));
        let adapter = match configured {
            Some(want) if adapter_exists(&want) => want,
            Some(want) => {
                // Keep telemetry running when a configured dongle disappears.
                tracing::warn!(
                    "configured BLE_ADAPTER={} not present; falling back to {}",
                    want,
                    DEFAULT_ADAPTER
                );
                DEFAULT_ADAPTER.to_string()
            }
            None => DEFAULT_ADAPTER.to_string(),
        };

        // Keep-accessory remains inert without explicit enablement.
        let ka_enabled = active
            .get("KEEP_ACCESSORY_ENABLED")
            .map(|v| matches!(v.trim(), "yes" | "true" | "1"))
            .unwrap_or(false);
        let home_lat = active
            .get("KEEP_ACCESSORY_HOME_LAT")
            .and_then(|s| s.trim().parse::<f64>().ok());
        let home_lon = active
            .get("KEEP_ACCESSORY_HOME_LON")
            .and_then(|s| s.trim().parse::<f64>().ok());
        let home_radius_m = active
            .get("KEEP_ACCESSORY_HOME_RADIUS_M")
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|r| *r > 0.0)
            .unwrap_or(DEFAULT_HOME_RADIUS_M);
        let ka_hold_for_charge = active
            .get("KEEP_ACCESSORY_HOLD_FOR_CHARGE")
            .map(|v| matches!(v.trim(), "yes" | "true" | "1"))
            .unwrap_or(false);
        let charge_grace_min = active
            .get("KEEP_ACCESSORY_CHARGE_GRACE_MIN")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|m| *m > 0)
            .unwrap_or(DEFAULT_CHARGE_GRACE_MIN);
        let keep_accessory = KeepAccessoryConfig {
            enabled: ka_enabled,
            home_lat,
            home_lon,
            home_radius_m,
            hold_for_charge: ka_hold_for_charge,
            charge_grace_min,
        };

        // Automatic Away Mode requires fresh GPS for the API watcher.
        let away_auto_enabled = active
            .get("AWAY_MODE_AUTO_ENABLED")
            .map(|v| matches!(v.trim(), "yes" | "true" | "1"))
            .unwrap_or(false);

        // Experimental telemetry is opt-in.
        let experimental = sentryusb_config::get_config_value(
            &active,
            &commented,
            "SENTRYUSB_EXPERIMENTAL",
        )
        .map(|v| matches!(v.as_str(), "yes" | "true" | "1"))
        .unwrap_or(false);

        // Clamp the nudge cadence to avoid radio spam or sleep-window overruns.
        let keep_awake_interval_secs = sentryusb_config::get_config_value(
            &active,
            &commented,
            "BLE_KEEP_AWAKE_INTERVAL_SEC",
        )
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| (15..=900).contains(s))
        .unwrap_or(DEFAULT_KEEP_AWAKE_INTERVAL_SECS);

        // Sampler must honor the keep-awake master switch; when off it skips
        // the CPC nudge (the bug was the sampler ignoring this flag). Read it
        // byte-exact via the same path as api/ble.rs so the sampler and web UI
        // agree, including when the key is missing. Legacy enablement is
        // materialized by the API startup migration; fresh telemetry-only
        // configurations must not implicitly enable keep-awake.
        let keep_awake_enabled = parse_keep_awake_enabled(
            sentryusb_config::get_config_value(
                &active,
                &commented,
                "BLE_KEEP_AWAKE_ENABLED",
            )
            .as_deref(),
        );

        Ok(Self {
            enabled,
            vin,
            adapter,
            keep_accessory,
            away_auto_enabled,
            experimental,
            keep_awake_interval_secs,
            keep_awake_enabled,
        })
    }
}

/// Parses `BLE_KEEP_AWAKE_ENABLED`. Byte-exact `yes`/`true`/`1` => enabled;
/// any other present value (e.g. `YES`, `On`, `no`, `" yes "`) => disabled;
/// absent => disabled, matching the API and awake_start. API startup migration
/// writes an explicit value for legacy configurations.
/// NO trim/case-fold — identical to api/ble.rs so the sampler and web UI can
/// never disagree on the same value.
fn parse_keep_awake_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => matches!(v, "yes" | "true" | "1"),
        None => false,
    }
}

/// Checks whether the configured Bluetooth adapter currently exists.
fn adapter_exists(adapter: &str) -> bool {
    std::path::Path::new(&format!("/sys/class/bluetooth/{adapter}")).exists()
}

#[cfg(test)]
mod tests {
    use super::{BleConfig, parse_keep_awake_enabled};

    #[test]
    fn keep_awake_missing_defaults_disabled() {
        assert!(!parse_keep_awake_enabled(None));
        assert!(!BleConfig::default().keep_awake_enabled);
    }

    #[test]
    fn enabling_telemetry_on_a_fresh_config_does_not_enable_keep_awake() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sentryusb.conf");
        std::fs::write(
            &path,
            "export BLE_ENABLED=yes\nexport TESLA_BLE_VIN=5YJ3E1EA0KF000001\n",
        )
        .unwrap();

        let cfg = BleConfig::load_from(path.to_str().unwrap()).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.vin, "5YJ3E1EA0KF000001");
        assert!(!cfg.keep_awake_enabled);
    }

    #[test]
    fn keep_awake_config_reloads_explicit_values_and_key_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sentryusb.conf");
        for (setting, expected) in [
            ("export BLE_KEEP_AWAKE_ENABLED=yes\n", true),
            ("export BLE_KEEP_AWAKE_ENABLED=no\n", false),
            ("export BLE_KEEP_AWAKE_ENABLED=yes\n", true),
            ("", false),
        ] {
            std::fs::write(&path, format!("export BLE_ENABLED=yes\n{setting}")).unwrap();
            let cfg = BleConfig::load_from(path.to_str().unwrap()).unwrap();
            assert!(cfg.enabled);
            assert_eq!(cfg.keep_awake_enabled, expected, "{setting:?}");
        }
    }

    #[test]
    fn keep_awake_affirmative_values_enable() {
        for v in ["yes", "true", "1"] {
            assert!(parse_keep_awake_enabled(Some(v)), "{v:?} should enable");
        }
    }

    #[test]
    fn keep_awake_off_and_unrecognized_disable() {
        // Byte-exact: explicit off, case variants, AND whitespace-padded values
        // all disable (the parser does not trim; matches api/ble.rs).
        for v in [
            "no", "false", "0", "off", "YES", "True", "On", "", " yes ", "true\n", " 1",
        ] {
            assert!(!parse_keep_awake_enabled(Some(v)), "{v:?} should disable");
        }
    }
}

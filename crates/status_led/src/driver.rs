//! Exclusive, reversible LED ownership, including Pi 5's inverted ACT ABI.
use crate::{model::Frame, observe::Paths};
use serde::{Deserialize, Serialize};
use std::os::{
    fd::AsRawFd,
    unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    name: String,
    trigger: String,
    brightness: u32,
    maximum: u32,
    attributes: BTreeMap<String, String>,
}
pub struct Driver {
    paths: Paths,
    saved: Vec<Saved>,
    lock: Option<fs::File>,
    last: Option<Frame>,
    invert_activity: bool,
}
pub(crate) fn ensure_run(run: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(run)?;
    let m = fs::symlink_metadata(run)?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
        return Err(io::Error::other("unsafe LED runtime directory"));
    }
    Ok(())
}
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    // All writers hold ownership or use process-specific files in a private directory.
    let tmp = path.with_extension(format!("new-{}", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(content)?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
fn lock(paths: &Paths) -> io::Result<Option<fs::File>> {
    ensure_run(&paths.run)?;
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(paths.run.join("owner.lock"))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(e);
    }
    Ok(Some(f))
}
fn capture(paths: &Paths, name: &str) -> io::Result<Saved> {
    let p = paths.leds.join(name);
    let text = fs::read_to_string(p.join("trigger"))?;
    let trigger = text
        .split_whitespace()
        .find_map(|s| s.strip_prefix('[')?.strip_suffix(']'))
        .or_else(|| {
            if text.split_whitespace().count() == 1 {
                Some(text.trim())
            } else {
                None
            }
        })
        .ok_or_else(|| io::Error::other("LED has no active trigger"))?
        .to_string();
    let brightness = fs::read_to_string(p.join("brightness"))?
        .trim()
        .parse::<u32>()
        .map_err(io::Error::other)?;
    let maximum = fs::read_to_string(p.join("max_brightness"))?
        .trim()
        .parse::<u32>()
        .map_err(io::Error::other)?;
    if maximum == 0 || brightness > maximum {
        return Err(io::Error::other("invalid LED brightness range"));
    }
    let mut attributes = BTreeMap::new();
    for attr in match trigger.as_str() {
        "timer" => vec!["delay_on", "delay_off"],
        "heartbeat" => vec!["invert"],
        _ => vec![],
    } {
        attributes.insert(attr.into(), fs::read_to_string(p.join(attr))?);
    }
    Ok(Saved {
        name: name.into(),
        trigger,
        brightness,
        maximum,
        attributes,
    })
}
fn restore_saved(paths: &Paths, saved: &[Saved]) -> io::Result<()> {
    let mut error = None;
    for s in saved {
        let r = (|| {
            if ![
                "ACT",
                "led0",
                "status",
                "user-led2",
                "radxa-zero:green",
                "PWR",
            ]
            .contains(&s.name.as_str())
            {
                return Err(io::Error::other("invalid saved LED name"));
            }
            let p = paths.leds.join(&s.name);
            fs::write(p.join("trigger"), "none")?;
            fs::write(p.join("brightness"), s.brightness.to_string())?;
            fs::write(p.join("trigger"), &s.trigger)?;
            for (a, v) in &s.attributes {
                if !["delay_on", "delay_off", "invert"].contains(&a.as_str()) {
                    return Err(io::Error::other("invalid saved LED attribute"));
                }
                fs::write(p.join(a), v)?;
            }
            Ok(())
        })();
        if let Err(e) = r {
            error = Some(e);
        }
    }
    if let Some(e) = error { Err(e) } else { Ok(()) }
}
fn restore_locked(paths: &Paths) -> io::Result<()> {
    let path = paths.run.join("restore.json");
    let content = match fs::read(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let saved: Vec<Saved> = serde_json::from_slice(&content).map_err(io::Error::other)?;
    restore_saved(paths, &saved)?;
    fs::remove_file(path)
}
fn pi5_activity_inverted(paths: &Paths) -> io::Result<bool> {
    // Pi 5's onboard green LED is electrically active-high. The stock DT
    // intentionally declares it active-low so mmc0 flashes *off* from a lit
    // idle. Consequently sysfs brightness 0 means physically on. Read the
    // effective DT so an act_led_activelow=off override is not inverted twice.
    // https://github.com/raspberrypi/linux/issues/6783
    let gpios = match fs::read(paths.leds.join("ACT/of_node/gpios")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::read(paths.model.with_file_name("leds").join("led-act/gpios"))?
        }
        Err(e) => return Err(e),
    };
    // One phandle and the two GPIO cells, all big-endian. GPIO offsets are
    // controller-local: pin 9 alone does not identify the onboard circuit.
    if gpios.len() != 12 {
        return Err(io::Error::other("unrecognized Pi 5 ACT GPIO description"));
    }
    let pin = u32::from_be_bytes(gpios[4..8].try_into().unwrap());
    let flags = u32::from_be_bytes(gpios[8..12].try_into().unwrap());
    if pin != 9 || flags > 1 {
        return Err(io::Error::other("unsupported Pi 5 ACT GPIO or polarity"));
    }
    // Resolve the reference against the effective phandle of BCM2712's known
    // gio_aon node, not a compiled-in phandle or an arbitrary GPIO provider.
    // The path, register range and binding are from bcm2712.dtsi; downstream
    // bcm2712-ds.dtsi retains brcm,brcmstb-gpio. Decline unknown layouts.
    let controller = paths
        .model
        .with_file_name("soc@107c000000")
        .join("gpio@7d517c00");
    let phandle = u32::from_be_bytes(gpios[..4].try_into().unwrap());
    let compatible = fs::read(controller.join("compatible"))?;
    if phandle == 0
        || phandle == u32::MAX
        || fs::read(controller.join("phandle"))? != gpios[..4]
        || fs::read(controller.join("#gpio-cells"))? != 2u32.to_be_bytes()
        || !fs::read(controller.join("gpio-controller"))?.is_empty()
        || fs::read(controller.join("reg"))?
            != [0x7d517c00u32.to_be_bytes(), 0x40u32.to_be_bytes()].concat()
        || !compatible
            .split_inclusive(|b| *b == 0)
            .any(|name| name == b"brcm,brcmstb-gpio\0")
    {
        return Err(io::Error::other("unsupported Pi 5 ACT GPIO controller"));
    }
    Ok(flags == 1)
}
impl Driver {
    pub(crate) fn available(paths: &Paths) -> bool {
        activity_led(paths).is_some()
    }
    pub fn claim(paths: &Paths) -> io::Result<Option<Self>> {
        let Some(name) = activity_led(paths) else {
            return Ok(None);
        };
        if !paths.archive_handoff_safe() {
            return Ok(None);
        }
        let Some(lock) = lock(paths)? else {
            return Ok(None);
        };
        // Recover an earlier crashed owner before taking a fresh native-state snapshot.
        restore_locked(paths)?;
        let mut saved = vec![capture(paths, name)?];
        let model = fs::read_to_string(&paths.model).unwrap_or_default();
        let pi5_activity = model
            .trim_end_matches('\0')
            .starts_with("Raspberry Pi 5 Model B")
            && name == "ACT";
        let invert_activity = pi5_activity && pi5_activity_inverted(paths)?;
        if pi5_activity && paths.leds.join("PWR").is_dir() {
            saved.push(capture(paths, "PWR")?);
        }
        atomic_write(
            &paths.run.join("restore.json"),
            &serde_json::to_vec(&saved).map_err(io::Error::other)?,
        )?;
        let owner = Self {
            paths: paths.clone(),
            saved,
            lock: Some(lock),
            last: None,
            invert_activity,
        };
        for s in &owner.saved {
            fs::write(paths.leds.join(&s.name).join("trigger"), "none")?;
        }
        Ok(Some(owner))
    }
    pub fn set(&mut self, frame: Frame) -> io::Result<()> {
        if self.last == Some(frame) {
            return Ok(());
        }
        let colour = self.saved.len() == 2;
        for (i, s) in self.saved.iter().enumerate() {
            let on = if i == 1 {
                frame.red
            } else if colour {
                frame.green
            } else {
                frame.green || frame.red
            };
            fs::write(
                self.paths.leds.join(&s.name).join("brightness"),
                if on != (i == 0 && self.invert_activity) {
                    s.maximum.to_string()
                } else {
                    "0".into()
                },
            )?;
        }
        self.last = Some(frame);
        Ok(())
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        if self.lock.is_some() && restore_saved(&self.paths, &self.saved).is_ok() {
            let _ = fs::remove_file(self.paths.run.join("restore.json"));
        }
    }
}
pub fn restore(paths: &Paths) -> io::Result<()> {
    if !paths.run.exists() || !paths.archive_handoff_safe() {
        return Ok(());
    }
    let Some(_lock) = lock(paths)? else {
        return Ok(());
    };
    restore_locked(paths)
}

fn activity_led(paths: &Paths) -> Option<&'static str> {
    ["ACT", "led0", "status", "user-led2", "radxa-zero:green"]
        .into_iter()
        .find(|name| paths.leds.join(name).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn led(p: &Path) {
        fs::create_dir_all(p).unwrap();
        fs::write(p.join("trigger"), "none [mmc0] timer").unwrap();
        fs::write(p.join("brightness"), "0").unwrap();
        fs::write(p.join("max_brightness"), "255").unwrap();
    }
    fn fixture(model: &str) -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let p = Paths {
            leds: r.join("leds"),
            model: r.join("dt/model"),
            run: r.join("run"),
            proc: r.join("proc"),
            archive_script: r.join("archiveloop"),
            ..Paths::default()
        };
        fs::create_dir_all(&p.proc).unwrap();
        fs::create_dir_all(p.model.parent().unwrap()).unwrap();
        fs::write(&p.model, model).unwrap();
        led(&p.leds.join("ACT"));
        led(&p.leds.join("PWR"));
        if model.starts_with("Raspberry Pi 5 Model B") {
            pi5_gpio(&p, 9, 1);
            pi5_controller(&p, 7);
        }
        (d, p)
    }
    fn brightness(p: &Paths, name: &str) -> String {
        fs::read_to_string(p.leds.join(name).join("brightness")).unwrap()
    }
    fn pi5_gpio(p: &Paths, pin: u32, active_low: u32) {
        let node = p.leds.join("ACT/of_node");
        fs::create_dir_all(&node).unwrap();
        let cells: Vec<u8> = [7u32, pin, active_low]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect();
        fs::write(node.join("gpios"), cells).unwrap();
    }
    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }
    fn pi5_controller(p: &Paths, phandle: u32) -> std::path::PathBuf {
        // BCM2712's gio_aon, from bcm2712.dtsi and bcm2712-ds.dtsi.
        let node = p
            .model
            .parent()
            .unwrap()
            .join("soc@107c000000/gpio@7d517c00");
        fs::create_dir_all(&node).unwrap();
        fs::write(node.join("phandle"), cells(&[phandle])).unwrap();
        fs::write(node.join("compatible"), b"brcm,brcmstb-gpio\0").unwrap();
        fs::write(node.join("gpio-controller"), b"").unwrap();
        fs::write(node.join("#gpio-cells"), cells(&[2])).unwrap();
        fs::write(node.join("reg"), cells(&[0x7d517c00, 0x40])).unwrap();
        node
    }
    fn gpio_source(p: &Paths, live_dt: bool) -> std::path::PathBuf {
        let sysfs = p.leds.join("ACT/of_node/gpios");
        if !live_dt {
            return sysfs;
        }
        let node = p.model.parent().unwrap().join("leds/led-act");
        fs::create_dir_all(&node).unwrap();
        fs::rename(sysfs, node.join("gpios")).unwrap();
        node.join("gpios")
    }
    fn assert_native_control(p: &Paths) {
        assert!(Driver::claim(p).is_err());
        assert!(!p.run.join("restore.json").exists());
        for name in ["ACT", "PWR"] {
            assert_eq!(
                fs::read_to_string(p.leds.join(name).join("trigger")).unwrap(),
                "none [mmc0] timer"
            );
            assert_eq!(brightness(p, name), "0");
        }
    }
    #[test]
    fn pi5_other_gpio_controller_pin_nine_is_rejected() {
        for live_dt in [false, true] {
            let (_d, p) = fixture("Raspberry Pi 5 Model B");
            let other = p.model.parent().unwrap().join("axi/pcie/rp1/gpio@d0000");
            fs::create_dir_all(&other).unwrap();
            fs::write(other.join("phandle"), cells(&[11])).unwrap();
            fs::write(other.join("compatible"), b"raspberrypi,rp1-gpio\0").unwrap();
            fs::write(other.join("gpio-controller"), b"").unwrap();
            fs::write(other.join("#gpio-cells"), cells(&[2])).unwrap();
            let source = gpio_source(&p, live_dt);
            fs::write(source, cells(&[11, 9, 1])).unwrap();
            assert_native_control(&p);
        }
    }
    #[test]
    fn pi5_unresolved_gpio_controller_is_rejected() {
        for live_dt in [false, true] {
            let (_d, p) = fixture("Raspberry Pi 5 Model B");
            let source = gpio_source(&p, live_dt);
            fs::write(source, cells(&[987, 9, 1])).unwrap();
            assert_native_control(&p);
        }
    }
    #[test]
    fn pi5_invalid_controller_identity_or_cell_layout_is_rejected() {
        for live_dt in [false, true] {
            for (property, data) in [
                ("compatible", Some(b"raspberrypi,rp1-gpio\0".to_vec())),
                ("#gpio-cells", Some(cells(&[3]))),
                ("#gpio-cells", Some(vec![2])),
                ("reg", Some(cells(&[0x7d508500, 0x40]))),
                ("gpio-controller", None),
                ("phandle", None),
                ("phandle", Some(vec![7])),
            ] {
                let (_d, p) = fixture("Raspberry Pi 5 Model B");
                let node = pi5_controller(&p, 7);
                gpio_source(&p, live_dt);
                match data {
                    Some(bytes) => fs::write(node.join(property), bytes).unwrap(),
                    None => fs::remove_file(node.join(property)).unwrap(),
                }
                assert_native_control(&p);
            }
        }
    }
    #[test]
    fn pi5_controller_phandle_is_resolved_from_the_effective_tree() {
        for live_dt in [false, true] {
            for phandle in [7, 42, 0x1234] {
                for active_low in [0, 1] {
                    let (_d, p) = fixture("Raspberry Pi 5 Model B");
                    pi5_controller(&p, phandle);
                    let source = gpio_source(&p, live_dt);
                    fs::write(source, cells(&[phandle, 9, active_low])).unwrap();
                    let mut owner = Driver::claim(&p).unwrap().unwrap();
                    let frame = Frame {
                        green: true,
                        red: true,
                    };
                    owner.set(frame).unwrap();
                    assert_eq!(visible_pi5(&p, active_low == 1), frame);
                }
            }
        }
    }
    fn visible_pi5(p: &Paths, active_low: bool) -> Frame {
        // Model the Pi 5 onboard circuit: green is electrically active-high;
        // stock DT deliberately reverses its logical sysfs brightness. Red
        // uses the normal logical brightness mapping.
        let green_requested = brightness(p, "ACT").trim() != "0";
        Frame {
            green: green_requested != active_low,
            red: brightness(p, "PWR").trim() != "0",
        }
    }
    #[test]
    fn pi5_connected_is_two_lit_flashes_with_a_dark_idle() {
        use crate::model::State;
        use std::time::Duration;
        let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
        pi5_gpio(&p, 9, 1);
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        for ms in [0, 99, 100, 249, 250, 349, 350, 2999, 3000] {
            let frame = State::Connected.frame(Duration::from_millis(ms));
            owner.set(frame).unwrap();
            assert_eq!(visible_pi5(&p, true), frame, "connected at {ms} ms");
        }
    }
    #[test]
    fn pi5_starting_and_preparing_are_amber_dark_not_red_green() {
        use crate::model::State;
        use std::time::Duration;
        let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
        pi5_gpio(&p, 9, 1);
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        for (state, samples) in [
            (State::Starting, [0, 999, 1000, 1999, 2000]),
            (State::Preparing, [0, 249, 250, 499, 500]),
        ] {
            for ms in samples {
                let frame = state.frame(Duration::from_millis(ms));
                owner.set(frame).unwrap();
                assert_eq!(visible_pi5(&p, true), frame, "{state:?} at {ms} ms");
            }
        }
    }
    #[test]
    fn pi5_respects_effective_dt_polarity_and_restores_raw_state() {
        for active_low in [0, 1] {
            let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
            pi5_gpio(&p, 9, active_low);
            let mut owner = Driver::claim(&p).unwrap().unwrap();
            for frame in [
                Frame {
                    green: false,
                    red: true,
                },
                Frame {
                    green: true,
                    red: false,
                },
                Frame {
                    green: false,
                    red: false,
                },
            ] {
                owner.set(frame).unwrap();
                assert_eq!(visible_pi5(&p, active_low == 1), frame);
            }
            drop(owner);
            assert_eq!(brightness(&p, "ACT"), "0");
            assert_eq!(brightness(&p, "PWR"), "0");
            assert_eq!(
                fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
                "mmc0"
            );
        }
    }
    #[test]
    fn pi5_can_read_polarity_from_live_dt_when_sysfs_has_no_of_node() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
        pi5_gpio(&p, 9, 1);
        let dt_node = p.model.parent().unwrap().join("leds/led-act");
        fs::create_dir_all(&dt_node).unwrap();
        fs::rename(p.leds.join("ACT/of_node/gpios"), dt_node.join("gpios")).unwrap();
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        let frame = Frame {
            green: true,
            red: true,
        };
        owner.set(frame).unwrap();
        assert_eq!(visible_pi5(&p, true), frame);
    }
    #[test]
    fn pi5_unknown_or_remapped_gpio_leaves_native_led_control_alone() {
        for data in [
            None,
            Some(vec![0, 1]),
            Some(
                [7u32, 10, 1]
                    .into_iter()
                    .flat_map(u32::to_be_bytes)
                    .collect(),
            ),
            Some(
                [7u32, 9, 3]
                    .into_iter()
                    .flat_map(u32::to_be_bytes)
                    .collect(),
            ),
        ] {
            let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
            let node = p.leds.join("ACT/of_node");
            fs::create_dir_all(&node).unwrap();
            if let Some(data) = data {
                fs::write(node.join("gpios"), data).unwrap();
            } else {
                let _ = fs::remove_file(node.join("gpios"));
            }
            assert!(Driver::claim(&p).is_err());
            assert!(!p.run.join("restore.json").exists());
            assert_eq!(
                fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
                "none [mmc0] timer"
            );
            assert_eq!(
                fs::read_to_string(p.leds.join("PWR/trigger")).unwrap(),
                "none [mmc0] timer"
            );
            assert_eq!(brightness(&p, "ACT"), "0");
            assert_eq!(brightness(&p, "PWR"), "0");
        }
    }
    #[test]
    fn pi5_can_show_amber_green_red_and_restores_original_triggers() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B Rev 1.0\0");
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        owner
            .set(Frame {
                green: true,
                red: true,
            })
            .unwrap();
        assert_eq!(brightness(&p, "ACT"), "0");
        assert_eq!(brightness(&p, "PWR"), "255");
        owner
            .set(Frame {
                green: true,
                red: false,
            })
            .unwrap();
        assert_eq!(brightness(&p, "PWR"), "0");
        owner
            .set(Frame {
                green: false,
                red: true,
            })
            .unwrap();
        assert_eq!(brightness(&p, "ACT"), "255");
        drop(owner);
        assert_eq!(
            fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
            "mmc0"
        );
        assert_eq!(brightness(&p, "PWR"), "0");
    }
    #[test]
    fn other_boards_never_take_power_led_and_red_fault_falls_back_to_green() {
        for model in ["Raspberry Pi 4 Model B", "Raspberry Pi Zero 2 W", "unknown"] {
            let (_d, p) = fixture(model);
            // An active-low declaration on another board is normally accurate.
            pi5_gpio(&p, 9, 1);
            let mut owner = Driver::claim(&p).unwrap().unwrap();
            owner
                .set(Frame {
                    green: false,
                    red: true,
                })
                .unwrap();
            assert_eq!(brightness(&p, "ACT"), "255");
            assert_eq!(
                fs::read_to_string(p.leds.join("PWR/trigger")).unwrap(),
                "none [mmc0] timer"
            );
            assert_eq!(brightness(&p, "PWR"), "0");
        }
    }
    #[test]
    fn second_owner_and_cleanup_cannot_override_live_owner() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B");
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        owner
            .set(Frame {
                green: true,
                red: true,
            })
            .unwrap();
        assert!(Driver::claim(&p).unwrap().is_none());
        restore(&p).unwrap();
        assert_eq!(brightness(&p, "ACT"), "0");
        assert_eq!(brightness(&p, "PWR"), "255");
        drop(owner);
        restore(&p).unwrap();
        assert_eq!(brightness(&p, "ACT"), "0");
        assert_eq!(brightness(&p, "PWR"), "0");
    }
    #[test]
    fn partial_claim_failure_rolls_back_activity_led() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B");
        fs::remove_file(p.leds.join("PWR/trigger")).unwrap();
        fs::create_dir(p.leds.join("PWR/trigger")).unwrap();
        assert!(Driver::claim(&p).is_err());
        assert_eq!(
            fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
            "none [mmc0] timer"
        );
    }
    #[test]
    fn missing_led_is_a_supported_noop() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths {
            leds: d.path().join("absent"),
            run: d.path().join("run"),
            ..Paths::default()
        };
        assert!(Driver::claim(&p).unwrap().is_none());
    }
    #[test]
    fn crash_snapshot_is_recovered_before_reacquiring() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B");
        let mut owner = Driver::claim(&p).unwrap().unwrap();
        owner
            .set(Frame {
                green: true,
                red: true,
            })
            .unwrap();
        // Simulate process death: release flock without running LED cleanup.
        drop(owner.lock.take());
        std::mem::forget(owner);
        let next = Driver::claim(&p).unwrap().unwrap();
        drop(next);
        assert_eq!(brightness(&p, "ACT"), "0");
        assert_eq!(brightness(&p, "PWR"), "0");
        assert_eq!(
            fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
            "mmc0"
        );
    }

    fn archive_process(p: &Paths, pid: u32, parent: u32, start: u64) {
        let dir = p.proc.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("stat"),
            format!(
                "{pid} (archiveloop) S {parent} {} {start} 0",
                vec!["0"; 17].join(" ")
            ),
        )
        .unwrap();
        fs::write(dir.join("cmdline"), b"bash\0/root/bin/archiveloop\0").unwrap();
    }
    #[test]
    fn upgrade_waits_for_legacy_worker_even_after_atomic_script_refresh() {
        let (_d, p) = fixture("Raspberry Pi 5 Model B");
        fs::write(&p.archive_script, "function slowblink () { echo timer; }\n").unwrap();
        archive_process(&p, 42, 1, 123);
        assert!(Driver::claim(&p).unwrap().is_none());
        let replacement = p.archive_script.with_extension("new");
        fs::write(&replacement, "# sentryusb-status-led: cooperative-v1\n").unwrap();
        fs::rename(replacement, &p.archive_script).unwrap();
        // The running old process still has its original code after replacement.
        assert!(Driver::claim(&p).unwrap().is_none());
        assert!(!p.run.join("restore.json").exists());
        assert_eq!(
            fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
            "none [mmc0] timer"
        );
        // A valid new worker must not hide a separate legacy worker.
        archive_process(&p, 83, 1, 200);
        archive_process(&p, 84, 83, 201);
        fs::create_dir_all(&p.run).unwrap();
        fs::write(p.run.join("archive"), "84 201 ready\n").unwrap();
        assert!(Driver::claim(&p).unwrap().is_none());
        fs::remove_dir_all(p.proc.join("42")).unwrap();
        // The cooperative worker and its flock/shell parent are safe together.
        let owner = Driver::claim(&p)
            .unwrap()
            .expect("new worker handed off LED control");
        drop(owner);
        assert_eq!(
            fs::read_to_string(p.leds.join("ACT/trigger")).unwrap(),
            "mmc0"
        );
    }
}

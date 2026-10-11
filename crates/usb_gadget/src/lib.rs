//! Linux configfs USB mass-storage gadget control.

pub mod cycle_lock;
pub mod reflink;
pub mod snapshot;
pub mod space;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tracing::info;

const GADGET_NAME: &str = "sentryusb";
// Match the shell setup's literal configfs dentry. Although the kernel parses
// `0x0409` and `0x409` identically, configfs retains the spelling and rejects a
// second dentry for the same language ID.
const LANG: &str = "0x409";
const CFG: &str = "c";

/// Disk images that can be exposed as USB mass storage LUNs.
const DISK_IMAGES: &[(&str, &str)] = &[
    ("/backingfiles/cam_disk.bin", "CAM"),
    ("/backingfiles/music_disk.bin", "MUSIC"),
    ("/backingfiles/lightshow_disk.bin", "LIGHTSHOW"),
    ("/backingfiles/boombox_disk.bin", "BOOMBOX"),
];

/// Find the configfs root mount point.
fn find_configfs_root() -> Result<PathBuf> {
    let mounts = fs::read_to_string("/proc/mounts")
        .context("failed to read /proc/mounts")?;
    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 3 && fields[2] == "configfs" {
            return Ok(PathBuf::from(fields[1]));
        }
    }
    bail!("configfs not mounted")
}

/// Write a string to a sysfs/configfs file.
fn write_file(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Create `link -> target`, replacing any stale entry at `link` first.
/// `symlink_metadata()` detects dangling links that `Path::exists()` would
/// follow and miss, preventing EEXIST when recreating the link.
#[cfg(unix)]
fn ensure_symlink(target: &Path, link: &Path) -> Result<()> {
    match link.symlink_metadata() {
        Ok(_) => fs::remove_file(link)
            .with_context(|| format!("failed to remove stale symlink {}", link.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("failed to stat {}", link.display())),
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("failed to symlink {} -> {}", link.display(), target.display()))
}

#[cfg(not(unix))]
fn ensure_symlink(_target: &Path, _link: &Path) -> Result<()> {
    bail!("USB gadget control requires Linux")
}

/// Get the SBC model and return the appropriate MaxPower value (mA).
fn get_max_power() -> u32 {
    let model = fs::read_to_string("/proc/device-tree/model").unwrap_or_default();
    let model = model.to_lowercase();
    if model.contains("pi 5") {
        600
    } else if model.contains("pi 4") {
        500
    } else if model.contains("pi 3") {
        300
    } else if model.contains("pi 2") || model.contains("zero 2") {
        200
    } else {
        100
    }
}

/// Machine-ID-derived serial: `SentryUSB-<hex sha256(machine-id)>`.
/// Its stability preserves the host's cached USB identity.
fn get_machine_serial() -> String {
    let mid = fs::read_to_string("/etc/machine-id").unwrap_or_default();
    let mid = mid.trim();
    if mid.is_empty() {
        return "SentryUSB-unknown".to_string();
    }
    let h = ring::digest::digest(&ring::digest::SHA256, mid.as_bytes());
    format!("SentryUSB-{}", hex::encode(h.as_ref()))
}

/// True if the primary LUN exposes a present, nonempty, writable image.
fn gadget_dir_is_complete(gadget: &Path) -> bool {
    let func = gadget.join("functions/mass_storage.0");
    let lun0_file = func.join("lun.0/file");
    match fs::read_to_string(&lun0_file) {
        Ok(s) => {
            let path = s.trim();
            !path.is_empty()
                && fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
                && fs::read_to_string(func.join("lun.0/ro")).is_ok_and(|ro| ro.trim() == "0")
        }
        Err(_) => false,
    }
}

/// Check the real recording mount before publishing any disk. A directory
/// beneath a failed mount must never be mistaken for available storage.
pub fn validate_storage() -> Result<()> {
    let mounts = fs::read_to_string("/proc/self/mounts")
        .context("cannot check recording storage mount")?;
    let require_cam = sentryusb_config::parse_file(sentryusb_config::find_config_path())
        .map(|(config, _)| camera_required(&config)).unwrap_or(true);
    validate_storage_at(&mounts, Path::new("/backingfiles"), require_cam)
}

fn camera_required(config: &sentryusb_config::SetupConfig) -> bool {
    let Some(raw) = config.get("CAM_SIZE").or_else(|| config.get("camsize")) else { return true; };
    let raw = raw.trim();
    // Percentage sizes use the setup fallback image, including "0%".
    !raw.is_empty() && raw.trim_end_matches(|c: char| "kKmMgGbB".contains(c)).parse::<f64>().ok() != Some(0.0)
}

fn validate_storage_at(mounts: &str, root: &Path, require_cam: bool) -> Result<()> {
    let mounted_rw = mounts.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.get(1).copied() == root.to_str()
            && fields.get(3).is_some_and(|options| options.split(',').any(|s| s == "rw"))
    });
    if !mounted_rw {
        bail!("recording storage is not mounted read/write at {}", root.display());
    }
    if require_cam && !root.join("cam_disk.bin").is_file() {
        bail!("configured camera disk is unavailable; refusing to present only optional media");
    }
    let mut found = false;
    for (image, _) in DISK_IMAGES {
        let path = root.join(Path::new(image).file_name().unwrap());
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).context("cannot inspect USB disk image"),
            Ok(meta) if !meta.is_file() || meta.len() == 0 => {
                bail!("USB disk image is empty or not a regular file: {}", path.display());
            }
            Ok(_) => {}
        }
        // Open without creating, truncating, or writing to the live image.
        fs::OpenOptions::new().read(true).write(true).open(&path)
            .with_context(|| format!("USB disk image is not readable/writable: {}", path.display()))?;
        found = true;
    }
    if !found {
        bail!("no usable USB disk images on recording storage; waiting for setup or storage recovery");
    }
    Ok(())
}

/// Enable the USB gadget through configfs.
pub fn enable() -> Result<()> {
    validate_storage()?;
    let configfs = find_configfs_root()?;
    let gadget = configfs.join("usb_gadget").join(GADGET_NAME);

    // Release the UDC from the legacy single-function gadget first.
    let _ = std::process::Command::new("modprobe")
        .args(["-q", "-r", "g_mass_storage"])
        .status();

    // Rebind complete configurations; rebuild incomplete ones so the device
    // cannot enumerate without a LUN.
    if gadget.exists() {
        if gadget_dir_is_complete(&gadget) {
            // Kernel 6.18+ closes LUN backing files on UDC unbind. Rewrite each
            // current path before rebinding; compact LUN numbering means it
            // cannot be reconstructed positionally from DISK_IMAGES.
            let func_dir = gadget.join("functions/mass_storage.0");
            for i in 0..DISK_IMAGES.len() {
                let lun_file = func_dir.join(format!("lun.{}/file", i));
                let current = match fs::read_to_string(&lun_file) {
                    Ok(s) => s.trim().to_string(),
                    Err(_) => continue,
                };
                if current.is_empty() || !Path::new(&current).exists() {
                    continue;
                }
                write_file(&lun_file, "\n")?;
                // Apply nofua while the medium is detached.
                let _ = fs::write(func_dir.join(format!("lun.{}/nofua", i)), "1");
                std::thread::sleep(std::time::Duration::from_secs(1));
                write_file(&lun_file, &current)?;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
            return bind_udc(&gadget);
        }
        info!("USB gadget dir exists but is incomplete — tearing down and rebuilding");
        disable()?;
    }

    // modprobe interprets a second name as a parameter, so load each module
    // separately.
    let _ = std::process::Command::new("modprobe")
        .arg("libcomposite")
        .status();
    let _ = std::process::Command::new("modprobe")
        .arg("usb_f_mass_storage")
        .status();

    let cfg_dir = gadget.join(format!("configs/{}.1", CFG));
    fs::create_dir_all(&cfg_dir)
        .with_context(|| format!("failed to create {}", cfg_dir.display()))?;

    write_file(&gadget.join("idVendor"), "0x1d6b")?;  // Linux Foundation
    write_file(&gadget.join("idProduct"), "0x0104")?;  // Composite Gadget
    write_file(&gadget.join("bcdDevice"), "0x0100")?;  // v1.0.0
    write_file(&gadget.join("bcdUSB"), "0x0200")?;     // USB 2.0

    let strings_dir = gadget.join(format!("strings/{}", LANG));
    fs::create_dir_all(&strings_dir)
        .with_context(|| format!("failed to create {}", strings_dir.display()))?;
    let cfg_strings = gadget.join(format!("configs/{}.1/strings/{}", CFG, LANG));
    fs::create_dir_all(&cfg_strings)
        .with_context(|| format!("failed to create {}", cfg_strings.display()))?;

    write_file(&strings_dir.join("serialnumber"), &get_machine_serial())?;
    write_file(&strings_dir.join("manufacturer"), "SentryUSB")?;
    write_file(&strings_dir.join("product"), "SentryUSB Composite Gadget")?;
    write_file(&cfg_strings.join("configuration"), "SentryUSB Config")?;

    write_file(
        &cfg_dir.join("MaxPower"),
        &get_max_power().to_string(),
    )?;

    let func_dir = gadget.join("functions/mass_storage.0");
    fs::create_dir_all(&func_dir)
        .with_context(|| format!("failed to create {}", func_dir.display()))?;

    let mut lun = 0;
    for (image_path, label) in DISK_IMAGES {
        if Path::new(image_path).exists() {
            let lun_dir = func_dir.join(format!("lun.{}", lun));
            // Some kernels do not create lun.0 with the function node.
            fs::create_dir_all(&lun_dir)
                .with_context(|| format!("failed to create lun.{} at {}", lun, lun_dir.display()))?;
            // Tesla FUA writes can exceed its SCSI timeout when configfs flushes
            // synchronously. Cache them instead; images are fsck'd each cycle.
            // Kernels without `nofua` must still expose the drive.
            if let Err(e) = write_file(&lun_dir.join("nofua"), "1") {
                tracing::warn!("could not set nofua on lun.{lun}: {e:#}");
            }
            write_file(&lun_dir.join("file"), image_path)?;

            let size = fs::metadata(image_path)
                .map(|m| format_size(m.len()))
                .unwrap_or_else(|_| "?".to_string());
            write_file(
                &lun_dir.join("inquiry_string"),
                &format!("SentryUSB {} {}", label, size),
            )?;

            lun += 1;
        }
    }

    // Replace any dangling link left by an interrupted teardown.
    ensure_symlink(&func_dir, &cfg_dir.join("mass_storage.0"))?;

    info!("USB gadget configured with {} LUN(s)", lun);

    if lun == 0 || !gadget_dir_is_complete(&gadget) {
        bail!("refusing to bind USB gadget without a populated primary disk");
    }

    // Kernel 6.18+ on rockchip64 needs three seconds for LUN attributes to
    // propagate before binding, or mass storage may activate with no medium.
    std::thread::sleep(std::time::Duration::from_secs(3));

    bind_udc(&gadget)
}

/// Bind the UDC, clearing stale bindings and retrying transient failures.
fn bind_udc(gadget: &Path) -> Result<()> {
    let udc = find_udc()?;
    let udc_path = gadget.join("UDC");

    // Clear any stale binding.
    let _ = fs::write(&udc_path, "");

    for attempt in 1..=5 {
        match fs::write(&udc_path, &udc) {
            Ok(()) => {
                // Sysfs can accept a write while rejecting the bind; verify it.
                match fs::read_to_string(&udc_path) {
                    Ok(s) if s.trim() == udc.trim() => {
                        info!("USB gadget bound to UDC: {}", udc);
                        return Ok(());
                    }
                    Ok(other) if attempt < 5 => {
                        info!(
                            "UDC bind attempt {} wrote {:?} but sysfs reads back {:?}; retrying",
                            attempt, udc, other.trim()
                        );
                        let _ = fs::write(&udc_path, "");
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Ok(other) => {
                        return Err(anyhow::anyhow!(
                            "UDC bind silently rejected: wrote {:?}, readback {:?}",
                            udc,
                            other.trim()
                        ));
                    }
                    Err(_) => {
                        // A successful write is authoritative if readback is unavailable.
                        info!("USB gadget bound to UDC: {} (readback failed)", udc);
                        return Ok(());
                    }
                }
            }
            Err(e) if attempt < 5 => {
                info!("UDC bind attempt {} failed ({}), retrying", attempt, e);
                let _ = fs::write(&udc_path, "");
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to bind UDC {}: {}", udc, e));
            }
        }
    }
    Ok(())
}

/// Disable the USB gadget by tearing down configfs.
pub fn disable() -> Result<()> {
    // Release the UDC from g_mass_storage before deactivating configfs.
    let _ = std::process::Command::new("modprobe")
        .args(["-q", "-r", "g_mass_storage"])
        .status();

    let configfs = find_configfs_root()?;
    let gadget = configfs.join("usb_gadget").join(GADGET_NAME);

    if !gadget.exists() {
        info!("USB gadget already disabled");
        return Ok(());
    }

    // Some configfs handlers reject a zero-byte write; ENODEV is harmless.
    let _ = fs::write(gadget.join("UDC"), "\n");

    // Detach the function first to avoid EBUSY on LUN files and the function.
    let cfg_dir = gadget.join(format!("configs/{}.1", CFG));
    let _ = fs::remove_file(cfg_dir.join("mass_storage.0"));
    let cfg_strings = cfg_dir.join(format!("strings/{}", LANG));
    let _ = fs::remove_dir(&cfg_strings);
    // Remove the legacy spelling so it cannot leave libcomposite pinned.
    let _ = fs::remove_dir(cfg_dir.join("strings/0x0409"));

    // Clear backing-file handles; cascade-cleaned LUN paths are harmless.
    let func_dir = gadget.join("functions/mass_storage.0");
    for i in 0..=4 {
        let _ = fs::write(func_dir.join(format!("lun.{}/file", i)), "\n");
    }

    // lun.0 is an implicit configfs child released with the function; trying
    // to remove it directly returns EPERM and can prevent full teardown.
    for i in 1..=4 {
        let _ = fs::remove_dir(func_dir.join(format!("lun.{}", i)));
    }
    let _ = fs::remove_dir(&func_dir);

    let _ = fs::remove_dir(&cfg_dir);
    let _ = fs::remove_dir(gadget.join(format!("strings/{}", LANG)));
    let _ = fs::remove_dir(gadget.join("strings/0x0409")); // legacy form — see above
    let _ = fs::remove_dir(&gadget);

    let _ = std::process::Command::new("modprobe")
        .args(["-r", "usb_f_mass_storage", "g_ether", "usb_f_ecm", "usb_f_rndis", "libcomposite"])
        .status();

    // Surface residue from best-effort, kernel-dependent teardown.
    if gadget.exists() {
        tracing::warn!(
            "disable() completed but {} still present (incomplete teardown)",
            gadget.display()
        );
    }

    info!("USB gadget disabled");
    Ok(())
}

/// True when the gadget is bound and its primary LUN is populated.
pub fn is_active() -> bool {
    let root = Path::new("/sys/kernel/config/usb_gadget/sentryusb");
    let udc_bound = fs::read_to_string(root.join("UDC"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !udc_bound {
        return false;
    }
    gadget_dir_is_complete(root)
}

/// Find the first available UDC (USB Device Controller).
fn find_udc() -> Result<String> {
    let udc_dir = Path::new("/sys/class/udc");
    if let Ok(entries) = fs::read_dir(udc_dir) {
        if let Some(entry) = entries.flatten().next() {
            return Ok(entry.file_name().to_string_lossy().to_string());
        }
    }
    bail!("no UDC found in /sys/class/udc")
}

/// Format a byte count as human-readable (e.g., "32G", "512M").
fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{}G", bytes / 1_073_741_824)
    } else if bytes >= 1_048_576 {
        format!("{}M", bytes / 1_048_576)
    } else if bytes >= 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{}B", bytes)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn storage_requires_a_real_writable_mount_and_nonempty_images() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rw = format!("disk {} xfs rw,noatime 0 0\n", root.display());
        let ro = rw.replace("rw,noatime", "ro,noatime");
        assert!(validate_storage_at(&rw, root, true).is_err(), "no media");
        let image = root.join("cam_disk.bin");
        fs::write(&image, b"recording image").unwrap();
        assert!(validate_storage_at("", root, true).is_err(), "stray image below a failed mount");
        assert!(validate_storage_at(&ro, root, true).is_err(), "read-only recording mount");
        validate_storage_at(&rw, root, true).unwrap();
        assert_eq!(fs::read(&image).unwrap(), b"recording image", "probe must not modify footage");
        fs::write(&image, b"").unwrap();
        assert!(validate_storage_at(&rw, root, true).is_err(), "empty image");
        fs::remove_file(&image).unwrap();
        fs::write(root.join("music_disk.bin"), b"music image").unwrap();
        assert!(validate_storage_at(&rw, root, true).is_err(), "music cannot stand in for missing camera disk");
        validate_storage_at(&rw, root, false).unwrap();
    }

    #[test]
    fn only_explicitly_disabled_camera_allows_optional_media_alone() {
        assert!(camera_required(&sentryusb_config::SetupConfig::new()));
        for (size, expected) in [("", false), ("0", false), ("0G", false), ("0%", true), ("32G", true), ("100%", true)] {
            let config = sentryusb_config::SetupConfig::from([("CAM_SIZE".into(), size.into())]);
            assert_eq!(camera_required(&config), expected, "{size}");
        }
    }

    #[test]
    fn complete_gadget_requires_a_present_writable_primary_image() {
        let dir = tempfile::tempdir().unwrap();
        let lun = dir.path().join("functions/mass_storage.0/lun.0");
        fs::create_dir_all(&lun).unwrap();
        let image = dir.path().join("cam.bin");
        fs::write(lun.join("file"), image.to_str().unwrap()).unwrap();
        fs::write(lun.join("ro"), "0\n").unwrap();
        assert!(!gadget_dir_is_complete(dir.path()));
        fs::write(&image, b"disk").unwrap();
        assert!(gadget_dir_is_complete(dir.path()));
        fs::write(lun.join("ro"), "1\n").unwrap();
        assert!(!gadget_dir_is_complete(dir.path()));
        fs::write(lun.join("ro"), "0\n").unwrap();
        fs::write(lun.join("file"), "\n").unwrap();
        assert!(!gadget_dir_is_complete(dir.path()));
    }

    #[test]
    fn ensure_symlink_creates_fresh_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        ensure_symlink(&target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn ensure_symlink_replaces_valid_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        ensure_symlink(&target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn ensure_symlink_replaces_dangling_link() {
        let dir = tempfile::tempdir().unwrap();
        let stale_target = dir.path().join("gone");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&stale_target, &link).unwrap();
        assert!(!link.exists(), "dangling link should report not-existent");
        let new_target = dir.path().join("real");
        fs::create_dir(&new_target).unwrap();
        ensure_symlink(&new_target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), new_target);
    }
}

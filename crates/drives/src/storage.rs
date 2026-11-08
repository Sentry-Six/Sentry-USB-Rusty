//! Is the footage partition really mounted where the drive database lives?
//!
//! When the footage filesystem fails to mount, `/backingfiles` is just an
//! empty directory on the SD card. Opening the database there creates a
//! new, empty one on the SD card, which makes the UI look healthy while the
//! car has no drive. Every canonical database open checks this first.

use std::path::Path;

/// Where the footage filesystem (and the canonical drive database) lives.
pub const FOOTAGE_MOUNT: &str = "/backingfiles";

/// Setup writes one of these when it completes; a finished install has a
/// footage partition even if its fstab line went missing.
const SETUP_FINISHED_PATHS: &[&str] = &[
    "/sentryusb/SENTRYUSB_SETUP_FINISHED",
    "/boot/firmware/SENTRYUSB_SETUP_FINISHED",
    "/boot/SENTRYUSB_SETUP_FINISHED",
];

/// Why the footage filesystem can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FootageUnavailable {
    #[error("the footage drive is not mounted at {FOOTAGE_MOUNT}")]
    NotMounted,
    #[error("the footage drive is mounted read-only at {FOOTAGE_MOUNT}")]
    ReadOnly,
    #[error("{FOOTAGE_MOUNT} holds a {found} filesystem, expected {expected}")]
    WrongFilesystem { expected: String, found: String },
    #[error("cannot tell whether the footage drive is mounted: {0}")]
    Unknown(String),
}

/// Check the live system. `Ok` means canonical database files may be opened
/// (or created) under [`FOOTAGE_MOUNT`].
pub fn check_footage() -> Result<(), FootageUnavailable> {
    let fstab = match std::fs::read_to_string("/etc/fstab") {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(FootageUnavailable::Unknown(format!("/etc/fstab: {e}"))),
    };
    let setup_finished = SETUP_FINISHED_PATHS.iter().any(|p| Path::new(p).exists());
    check_footage_with(fstab.as_deref(), setup_finished, || {
        std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| format!("/proc/self/mountinfo: {e}"))
    })
}

/// Pure core of [`check_footage`]. `mountinfo` is only read when the install
/// is provisioned, so a dev box without `/proc` still passes.
pub fn check_footage_with(
    fstab: Option<&str>,
    setup_finished: bool,
    mountinfo: impl FnOnce() -> Result<String, String>,
) -> Result<(), FootageUnavailable> {
    let expected = fstab.and_then(fstab_fstype);
    if expected.is_none() && !setup_finished {
        // Not set up yet (fresh install, dev box): nothing to protect.
        return Ok(());
    }
    let mountinfo = mountinfo().map_err(FootageUnavailable::Unknown)?;
    let mounts = parse_mountinfo(&mountinfo);
    let Some(root) = mounts.iter().rev().find(|m| m.target == "/") else {
        return Err(FootageUnavailable::Unknown("no root filesystem in mountinfo".into()));
    };
    // The last mount at a path is the one visible there.
    let Some(footage) = mounts.iter().rev().find(|m| m.target == FOOTAGE_MOUNT) else {
        return Err(FootageUnavailable::NotMounted);
    };
    if footage.dev == root.dev || footage.root != "/" {
        // A bind of the root filesystem (or of a subdirectory) is still the SD card.
        return Err(FootageUnavailable::NotMounted);
    }
    // `auto` probes the type; `none` is a bind, which shows the bound
    // filesystem's own type.
    if let Some(want) = expected.filter(|t| *t != "auto" && *t != "none") {
        if footage.fstype != want {
            return Err(FootageUnavailable::WrongFilesystem {
                expected: want.to_string(),
                found: footage.fstype.clone(),
            });
        }
    }
    if !footage.options.split(',').any(|o| o == "rw") {
        return Err(FootageUnavailable::ReadOnly);
    }
    Ok(())
}

/// The device numbers (`maj:min`) of whatever is visible at each mountpoint,
/// from mountinfo text. Later mounts hide earlier ones.
pub fn visible_mounts(mountinfo: &str) -> std::collections::HashMap<String, String> {
    parse_mountinfo(mountinfo).into_iter().map(|m| (m.target, m.dev)).collect()
}

/// Whether a filesystem with device number `dev` (`maj:min`) is mounted
/// anywhere, from mountinfo text.
pub fn device_is_mounted(mountinfo: &str, dev: &str) -> bool {
    parse_mountinfo(mountinfo).iter().any(|m| m.dev == dev)
}

/// True when `target` is its own filesystem, not a directory on the SD
/// card's root. Used before writing state that must not land on the root.
pub fn is_separate_mount(target: &str) -> bool {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else { return false };
    let mounts = visible_mounts(&text);
    match (mounts.get("/"), mounts.get(target)) {
        (Some(root), Some(dev)) => root != dev,
        _ => false,
    }
}

/// The filesystem type of the active fstab entry for [`FOOTAGE_MOUNT`].
fn fstab_fstype(fstab: &str) -> Option<&str> {
    fstab
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 3 && f[1].trim_end_matches('/') == FOOTAGE_MOUNT { Some(f[2]) } else { None }
        })
        .next_back()
}

struct MountEntry {
    dev: String,
    root: String,
    target: String,
    options: String,
    fstype: String,
}

fn unescape(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let (pre, post) = line.split_once(" - ")?;
            let pre: Vec<&str> = pre.split_whitespace().collect();
            let post: Vec<&str> = post.split_whitespace().collect();
            if pre.len() < 6 || post.is_empty() {
                return None;
            }
            Some(MountEntry {
                dev: pre[2].to_string(),
                root: unescape(pre[3]),
                target: unescape(pre[4]),
                options: pre[5].to_string(),
                fstype: post[0].to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSTAB: &str = "proc /proc proc defaults 0 0\n\
        PARTUUID=abc-02 / ext4 defaults,noatime,ro 0 1\n\
        LABEL=backingfiles /backingfiles xfs auto,rw,noatime,nofail 0 2\n\
        LABEL=mutable /mutable ext4 auto,rw,nofail 0 2\n";
    const ROOT: &str = "22 1 179:2 / / ro,noatime shared:1 - ext4 /dev/mmcblk0p2 ro\n";
    const SSD: &str = "40 22 8:2 / /backingfiles rw,noatime shared:20 - xfs /dev/sda2 rw,attr2\n";
    const SD_ONLY: &str = "40 22 179:3 / /backingfiles rw,noatime shared:20 - xfs /dev/mmcblk0p3 rw\n";

    fn check(fstab: Option<&str>, finished: bool, mi: &str) -> Result<(), FootageUnavailable> {
        let mi = mi.to_string();
        check_footage_with(fstab, finished, move || Ok(mi))
    }

    #[test]
    fn external_ssd_and_sd_only_layouts_pass() {
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{SSD}")), Ok(()));
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{SD_ONLY}")), Ok(()));
    }

    #[test]
    fn unmounted_footage_is_refused() {
        assert_eq!(check(Some(FSTAB), true, ROOT), Err(FootageUnavailable::NotMounted));
        // Without the setup marker the fstab line alone still counts.
        assert_eq!(check(Some(FSTAB), false, ROOT), Err(FootageUnavailable::NotMounted));
        // A finished install whose fstab line is gone is still protected.
        assert_eq!(check(Some("proc /proc proc defaults 0 0\n"), true, ROOT), Err(FootageUnavailable::NotMounted));
    }

    #[test]
    fn fresh_install_and_dev_boxes_are_allowed_without_reading_mounts() {
        let unreadable = || Err("no /proc".to_string());
        assert_eq!(check_footage_with(None, false, unreadable), Ok(()));
        assert_eq!(check_footage_with(Some("proc /proc proc defaults 0 0\n"), false, unreadable), Ok(()));
        // A commented-out line doesn't count as provisioned.
        assert_eq!(check_footage_with(Some("#LABEL=backingfiles /backingfiles xfs rw 0 2\n"), false, unreadable), Ok(()));
    }

    #[test]
    fn provisioned_but_unreadable_mounts_is_unknown_not_allowed() {
        let r = check_footage_with(Some(FSTAB), true, || Err("denied".into()));
        assert!(matches!(r, Err(FootageUnavailable::Unknown(_))));
    }

    #[test]
    fn a_bind_of_the_sd_card_is_not_the_footage_drive() {
        let same_dev = "41 22 179:2 / /backingfiles rw shared:21 - ext4 /dev/mmcblk0p2 rw\n";
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{same_dev}")), Err(FootageUnavailable::NotMounted));
        let subdir = "41 22 8:2 /some/dir /backingfiles rw shared:21 - xfs /dev/sda2 rw\n";
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{subdir}")), Err(FootageUnavailable::NotMounted));
    }

    #[test]
    fn wrong_type_or_read_only_is_refused() {
        let ext4 = "40 22 8:2 / /backingfiles rw shared:20 - ext4 /dev/sda2 rw\n";
        assert!(matches!(check(Some(FSTAB), true, &format!("{ROOT}{ext4}")), Err(FootageUnavailable::WrongFilesystem { .. })));
        let ro = "40 22 8:2 / /backingfiles ro,noatime shared:20 - xfs /dev/sda2 ro\n";
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{ro}")), Err(FootageUnavailable::ReadOnly));
    }

    #[test]
    fn a_truncated_fstab_line_does_not_panic() {
        let fstab = "LABEL=backingfiles /backingfiles xfs rw 0 2\nLABEL=mutable /mutable\n/backingfiles\n";
        assert_eq!(check(Some(fstab), false, &format!("{ROOT}{SSD}")), Ok(()));
        assert_eq!(check(Some("x /backingfiles\n"), false, ROOT), Ok(()));
    }

    #[test]
    fn a_bind_entry_accepts_the_bound_filesystem() {
        let fstab = "/mnt/footage /backingfiles none bind 0 0\n";
        assert_eq!(check(Some(fstab), true, &format!("{ROOT}{SSD}")), Ok(()));
        assert_eq!(check(Some(fstab), true, ROOT), Err(FootageUnavailable::NotMounted));
    }

    #[test]
    fn the_visible_mount_is_the_last_one() {
        // Footage mounted, then something mounted over it from the SD card.
        let over = "50 40 179:2 / /backingfiles rw shared:30 - ext4 /dev/mmcblk0p2 rw\n";
        assert_eq!(check(Some(FSTAB), true, &format!("{ROOT}{SSD}{over}")), Err(FootageUnavailable::NotMounted));
    }
}

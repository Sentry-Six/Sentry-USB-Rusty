//! Stage a pinned automatic release before changing any executable.
use anyhow::{Context, Result, ensure};
use std::{io::{Read, Write}, path::{Path, PathBuf}, time::Duration};
use crate::update_attempt::{self, Attempt};

pub(super) struct Staged {
    files: Vec<(PathBuf, PathBuf)>,
    binary: PathBuf,
}

pub(super) async fn arm_shutdown_receipt(a: &Attempt) -> Result<()> {
    let dir = PathBuf::from("/opt/sentryusb").join(format!("stage-{}", a.id));
    let script = dir.join("confirm-shutdown.py");
    let source = SHUTDOWN_RECEIPT.replace("ATTEMPT_FILE", &serde_json::to_string(&update_attempt::current_path())?);
    std::fs::write(&script, source)?;
    std::fs::File::open(&script)?.sync_all()?;
    // Stopping a stale handoff service during normal operation cannot write a
    // receipt: the helper requires a real reboot.target shutdown transaction.
    let _ = sentryusb_shell::run("systemctl", &["stop", "sentryusb-auto-update-handoff.service"]).await;
    let _ = sentryusb_shell::run("systemctl", &["reset-failed", "sentryusb-auto-update-handoff.service"]).await;
    let stop = format!("--property=ExecStop=/usr/bin/python3 {}", script.display());
    sentryusb_shell::run("systemd-run", &["--unit=sentryusb-auto-update-handoff", "--property=Type=oneshot",
        "--property=RemainAfterExit=yes", "--property=TimeoutStopSec=15", &stop, "/usr/bin/true"]).await?;
    Ok(())
}

const SHUTDOWN_RECEIPT: &str = r#"import json,os,pathlib,subprocess
p=pathlib.Path(ATTEMPT_FILE)
a=json.loads(p.read_text())
jobs=subprocess.check_output(['systemctl','list-jobs','--no-legend','--no-pager'],text=True)
if a['phase']!='awaiting_reboot' or not any(len(j.split())>=3 and j.split()[1:3]==['reboot.target','start'] for j in jobs.splitlines()): raise SystemExit(0)
boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
if boot!=a['boot_id']: raise SystemExit(0)
receipt=p.with_name(a['id']+'.shutdown.json')
temp=receipt.with_suffix('.new')
with temp.open('x') as f:
 json.dump({'id':a['id'],'boot_id':boot},f);f.flush();os.fsync(f.fileno())
os.replace(temp,receipt)
fd=os.open(receipt.parent,os.O_RDONLY);os.fsync(fd);os.close(fd)
"#;

pub(super) fn sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = [0u8; 65536];
    loop { let n = file.read(&mut buf)?; if n == 0 { break; } hash.update(&buf[..n]); }
    Ok(hex::encode(hash.finish().as_ref()))
}

async fn download(url: &str, dest: &Path) -> Result<()> {
    let mut response = crate::http_client().get(url).timeout(Duration::from_secs(300))
        .header("User-Agent", "sentryusb-auto-update").send().await?.error_for_status()?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(dest)?;
    let mut total = 0usize;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
        ensure!(total <= 512 * 1024 * 1024, "Update asset exceeds size limit");
        file.write_all(&chunk)?;
    }
    ensure!(total > 0, "Empty update asset");
    file.sync_all()?;
    Ok(())
}

pub(super) fn cleanup_uninstalled(root: &Path, id: &str) -> Result<()> {
    ensure!(id.starts_with("auto-update-") && id.len() < 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "Invalid staging identity");
    let dir = root.join(format!("stage-{id}"));
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            ensure!(meta.is_dir() && !meta.file_type().is_symlink(), "Unexpected staging directory type");
            ensure!(dir.canonicalize()?.parent() == Some(root.canonicalize()?.as_path()), "Staging directory escaped installation root");
            std::fs::remove_dir_all(dir)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn validate_elf(path: &Path, suffix: &str) -> Result<()> {
    let mut header = [0u8; 20];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    let (class, machine) = if suffix == "linux-armv7" { (1, 40) }
        else if suffix == "linux-amd64" { (2, 62) } else { (2, 183) };
    ensure!(&header[..4] == b"\x7fELF" && header[4] == class && header[5] == 1 &&
        u16::from_le_bytes([header[18], header[19]]) == machine, "Invalid executable or architecture mismatch");
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

pub(super) async fn stage(hub: &sentryusb_ws::Hub, a: &Attempt) -> Result<Staged> {
    let suffix = crate::update::detect_release_suffix().await?;
    let repo = crate::update::update_repo();
    let release: serde_json::Value = crate::http_client()
        .get(format!("https://api.github.com/repos/{repo}/releases/tags/{}", a.target))
        .header("User-Agent", "sentryusb-auto-update").timeout(Duration::from_secs(15))
        .send().await?.error_for_status()?.json().await?;
    ensure!(release["draft"] == false && release["prerelease"] == false && release["tag_name"] == a.target,
        "Selected release is no longer a published stable release");
    sentryusb_shell::run("/root/bin/remountfs_rw", &[]).await?;
    let free = sentryusb_shell::run("df", &["-Pk", "/opt/sentryusb"]).await?;
    let available = free.lines().nth(1).and_then(|l| l.split_whitespace().nth(3))
        .and_then(|v| v.parse::<u64>().ok()).context("Could not check update free space")?;
    ensure!(available >= 768 * 1024, "At least 768 MiB free space is required for staged automatic updates");
    let dir = PathBuf::from("/opt/sentryusb").join(format!("stage-{}", a.id));
    std::fs::create_dir(&dir)?;
    hub.broadcast("update_status", &serde_json::json!({"status":"downloading", "message":format!("Preparing {}", a.target)}));
    let mut files = Vec::new();
    for component in ["sentryusb", "sentryusb-tesla-telemetry", "sentryusb-ble-action"] {
        let name = format!("{component}-{suffix}");
        let staged = dir.join(&name);
        download(&format!("https://github.com/{repo}/releases/download/{}/{name}", a.target), &staged).await?;
        let asset = release["assets"].as_array().and_then(|assets| assets.iter().find(|asset| asset["name"] == name))
            .context("Required release asset is not published")?;
        if let Some(digest) = asset["digest"].as_str() {
            let expected = digest.strip_prefix("sha256:").context("Unsupported release digest")?;
            ensure!(sha256(&staged)? == expected, "Release asset checksum mismatch");
        }
        validate_elf(&staged, &suffix)?;
        let dest = if component == "sentryusb" && !Path::new("/opt/sentryusb/sentryusb-current").exists() {
            PathBuf::from("/opt/sentryusb/sentryusb")
        } else { PathBuf::from("/opt/sentryusb").join(&name) };
        files.push((staged, dest));
    }
    let binary = files[0].0.clone();
    #[cfg(unix)]
    for component in ["sentryusb-tesla-telemetry", "sentryusb-ble-action"] {
        let name = format!("{component}-{suffix}");
        if !files.iter().any(|(_, d)| d.file_name().is_some_and(|n| n == name.as_str())) { continue; }
        let dest = PathBuf::from("/root/bin").join(component);
        // Stage the symlink on its destination filesystem for legacy images.
        let link = dest.with_file_name(format!(".{component}-{}", a.id));
        std::os::unix::fs::symlink(format!("/opt/sentryusb/{name}"), &link)?;
        files.push((link, dest));
    }
    let source = dir.join("source.tar.gz");
    download(&format!("https://github.com/{repo}/archive/refs/tags/{}.tar.gz", a.target), &source).await?;
    // Validate the pinned payload before committing, including shell syntax.
    // Extract only regular runtime scripts; never honor archive links/paths.

    sentryusb_shell::run_with_timeout(Duration::from_secs(60), "python3", &["-c", EXTRACT_RELEASE,
        source.to_str().context("Invalid staging path")?, dir.to_str().context("Invalid staging path")?]).await?;
    files.push((source, PathBuf::from("/opt/sentryusb/auto-update-source.tar.gz")));
    let version = dir.join("version");
    std::fs::write(&version, &a.target)?;
    std::fs::File::open(&version)?.sync_all()?;
    files.push((version, PathBuf::from("/opt/sentryusb/version")));
    Ok(Staged { files, binary })
}

pub(super) async fn commit(staged: &Staged, a: &mut Attempt) -> Result<()> {
    let manifest = staged.binary.parent().context("Missing staging directory")?.join("replacement-plan.json");
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&manifest)?;
    file.write_all(&serde_json::to_vec(&staged.files)?)?;
    file.sync_all()?;
    sync_parent(&manifest)?;
    sync_parent(staged.binary.parent().context("Missing staging directory")?)?;
    a.binary_sha256 = sha256(&staged.binary)?;
    a.phase = "installing".into();
    update_attempt::save(&update_attempt::current_path(), a)?;
    replace_files(&staged.files)
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or_else(|| std::io::Error::other("Missing parent directory"))?)?.sync_all()?;
    Ok(())
}

fn replace_files(files: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut replaced: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = (|| -> Result<()> {
        for (source, dest) in files {
            let backup = if std::fs::symlink_metadata(dest).is_ok() {
                let path = source.with_extension("previous");
                #[cfg(unix)]
                if std::fs::symlink_metadata(dest)?.file_type().is_symlink() {
                    std::os::unix::fs::symlink(std::fs::read_link(dest)?, &path)?;
                } else {
                    std::fs::copy(dest, &path)?;
                    std::fs::File::open(&path)?.sync_all()?;
                }
                #[cfg(not(unix))]
                { std::fs::copy(dest, &path)?; std::fs::File::open(&path)?.sync_all()?; }
                // Persist the recovery name before replacing the live name.
                sync_parent(&path)?;
                Some(path)
            } else { None };
            std::fs::rename(source, dest)?;
            replaced.push((dest.clone(), backup));
            sync_parent(dest)?;
            sync_parent(source)?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (dest, backup) in replaced.iter().rev() {
            let rollback = if let Some(backup) = backup { std::fs::rename(backup, dest) }
                else { std::fs::remove_file(dest) };
            if let Err(e) = rollback { tracing::error!("Automatic update rollback failed for {}: {e}", dest.display()); }
            if let Err(e) = sync_parent(dest).and_then(|_| backup.as_ref().map_or(Ok(()), |p| sync_parent(p))) {
                tracing::error!("Automatic update rollback flush failed: {e}");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn legacy_and_variant_replacement_preserve_recovery_copies_and_user_files() {
        use std::os::unix::fs::symlink;
        for variant in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let stage = d.path().join("stage"); std::fs::create_dir(&stage).unwrap();
            let binary = d.path().join(if variant { "sentryusb-linux-amd64" } else { "sentryusb" });
            std::fs::write(&binary, "old binary").unwrap();
            let current = d.path().join("sentryusb-current");
            if variant { symlink(&binary, &current).unwrap(); }
            let user = d.path().join("footage.mp4"); std::fs::write(&user, "original footage").unwrap();
            let config = d.path().join("sentryusb.conf"); std::fs::write(&config, "original settings").unwrap();
            let source = stage.join("new-binary"); std::fs::write(&source, "new binary").unwrap();
            let old_helper = d.path().join("old-helper"); std::fs::write(&old_helper, "old helper").unwrap();
            let new_helper = d.path().join("new-helper"); std::fs::write(&new_helper, "new helper").unwrap();
            let helper = d.path().join("helper"); symlink(&old_helper, &helper).unwrap();
            let new_link = stage.join("helper-link"); symlink(&new_helper, &new_link).unwrap();
            replace_files(&[(source.clone(), binary.clone()), (new_link.clone(), helper.clone())]).unwrap();
            assert_eq!(std::fs::read_to_string(if variant { &current } else { &binary }).unwrap(), "new binary");
            assert_eq!(std::fs::read_to_string(source.with_extension("previous")).unwrap(), "old binary");
            assert_eq!(std::fs::read_link(new_link.with_extension("previous")).unwrap(), old_helper);
            assert_eq!(std::fs::read_to_string(helper).unwrap(), "new helper");
            assert_eq!(std::fs::read_to_string(user).unwrap(), "original footage");
            assert_eq!(std::fs::read_to_string(config).unwrap(), "original settings");
        }
    }
    #[test]
    fn shutdown_evidence_requires_reboot_job_matching_boot_and_completed_install() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("attempt.json");
        let boot = d.path().join("boot-id");
        std::fs::write(&boot, "old-boot\n").unwrap();
        for (phase, origin, jobs, expected) in [
            ("awaiting_reboot", "old-boot", "12 reboot.target start waiting", true),
            ("awaiting_reboot", "old-boot", "", false),
            ("installing", "old-boot", "12 reboot.target start waiting", false),
            ("awaiting_reboot", "other-boot", "12 reboot.target start waiting", false),
            ("awaiting_reboot", "old-boot", "12 shutdown.target start waiting", false),
        ] {
            let mut a = Attempt::new(origin, "cycle", "v1.0.0", "v2.0.0");
            a.phase = phase.into();
            std::fs::write(&p, serde_json::to_vec(&a).unwrap()).unwrap();
            let receipt = p.with_file_name(format!("{}.shutdown.json", a.id));
            let _ = std::fs::remove_file(&receipt);
            let script = SHUTDOWN_RECEIPT
                .replace("ATTEMPT_FILE", &serde_json::to_string(&p).unwrap())
                .replace("'/proc/sys/kernel/random/boot_id'", &serde_json::to_string(&boot).unwrap())
                .replace("subprocess.check_output(['systemctl','list-jobs','--no-legend','--no-pager'],text=True)", &serde_json::to_string(jobs).unwrap());
            assert!(std::process::Command::new("python3").current_dir(d.path()).args(["-c", &script]).status().unwrap().success());
            assert_eq!(receipt.exists(), expected, "{phase}, {origin}, {jobs}");
        }
    }
    #[test]
    fn asset_validation_rejects_html_wrong_architecture_and_truncation() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("binary");
        std::fs::write(&p, b"<html>release not found</html>").unwrap();
        assert!(validate_elf(&p, "linux-amd64").is_err());
        let mut h = [0u8; 20]; h[..4].copy_from_slice(b"\x7fELF");h[4]=2;h[5]=1;h[18]=183;
        std::fs::write(&p, h).unwrap();
        assert!(validate_elf(&p, "linux-amd64").is_err());
        assert!(validate_elf(&p, "linux-arm64-a76").is_ok());
        std::fs::write(&p, b"\x7fELF").unwrap();
        assert!(validate_elf(&p, "linux-arm64-a76").is_err());
    }
    #[test]
    fn replacement_failure_restores_previous_components_and_keeps_footage() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("old"); let new = d.path().join("new"); let footage = d.path().join("clip.mp4");
        std::fs::write(&old, "old binary").unwrap(); std::fs::write(&new, "new binary").unwrap();
        std::fs::write(&footage, "original footage").unwrap();
        assert!(replace_files(&[(new, old.clone()), (d.path().join("missing"), d.path().join("other"))]).is_err());
        assert_eq!(std::fs::read_to_string(old).unwrap(), "old binary");
        assert_eq!(std::fs::read_to_string(footage).unwrap(), "original footage");
    }
    #[test]
    fn staging_cleanup_cannot_remove_adjacent_files_or_follow_a_link() {
        let d = tempfile::tempdir().unwrap();
        let untouched = d.path().join("clip.mp4");
        std::fs::write(&untouched, "footage").unwrap();
        let stage = d.path().join("stage-auto-update-1"); std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("partial"), "download").unwrap();
        cleanup_uninstalled(d.path(), "auto-update-1").unwrap();
        assert!(!stage.exists()); assert!(untouched.exists());
        assert!(cleanup_uninstalled(d.path(), "../clip.mp4").is_err());
        #[cfg(unix)] {
            let elsewhere = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), &stage).unwrap();
            assert!(cleanup_uninstalled(d.path(), "auto-update-1").is_err());
            assert!(elsewhere.path().exists());
        }
    }
    #[test]
    fn github_shaped_source_tarball_with_root_directory_is_accepted() {
        let d = tempfile::tempdir().unwrap(); let tar = d.path().join("source.tar.gz");
        let fixture = r#"import io,sys,tarfile
with tarfile.open(sys.argv[1],'w:gz') as t:
 root=tarfile.TarInfo('repository-v4.0.0/');root.type=tarfile.DIRTYPE;t.addfile(root)
 for name in ['run/archiveloop','run/archive-control.sh','run/post-archive-process.sh','setup/pi/apply-runtime-patches.sh']:
  data=b'#!/bin/bash\ntrue\n';m=tarfile.TarInfo('repository-v4.0.0/'+name);m.size=len(data);t.addfile(m,io.BytesIO(data))
"#;
        assert!(std::process::Command::new("python3").current_dir(d.path()).args(["-c", fixture]).arg(&tar).status().unwrap().success());
        let out = d.path().join("extracted");
        assert!(std::process::Command::new("python3").current_dir(d.path()).args(["-c", EXTRACT_RELEASE]).arg(&tar).arg(&out).status().unwrap().success());
        assert_eq!(std::fs::read(out.join("run/archiveloop")).unwrap(), b"#!/bin/bash\ntrue\n");
    }
}

const EXTRACT_RELEASE: &str = r#"import pathlib,sys,tarfile,subprocess
archive,out=sys.argv[1:]
out=pathlib.Path(out)
found=set()
with tarfile.open(archive,'r:gz') as t:
 for m in t:
  parts=pathlib.PurePosixPath(m.name).parts
  if '..' in parts or m.name.startswith('/'): raise ValueError('unsafe release path')
  if len(parts)==1 and m.isdir(): continue
  if len(parts)<2: raise ValueError('unsafe release path')
  rel=pathlib.PurePosixPath(*parts[1:])
  if str(rel).startswith('run/') or str(rel)=='setup/pi/apply-runtime-patches.sh':
   if m.isdir(): continue
   if not m.isfile(): raise ValueError('runtime links not allowed')
   p=out/rel;p.parent.mkdir(parents=True,exist_ok=True)
   p.write_bytes(t.extractfile(m).read());found.add(str(rel))
   if p.read_bytes().startswith(b'#!/bin/bash'): subprocess.run(['bash','-n',str(p)],check=True)
for name in ['run/archiveloop','run/archive-control.sh','run/post-archive-process.sh','setup/pi/apply-runtime-patches.sh']:
 if name not in found: raise ValueError('incomplete release payload: '+name)
"#;

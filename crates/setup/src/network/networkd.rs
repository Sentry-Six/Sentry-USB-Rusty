//! Opt-in by platform/backend detection, never by the absence of NetworkManager alone.

use std::path::Path;
use std::time::Duration;
use anyhow::{Context, Result, bail};
use super::ap_profile::ApProfile;
use crate::{SetupEmitter, env::SetupEnv};

const SCRIPT: &str = include_str!("../../../../setup/pi/networkd-ap.sh");
const DIRECTORY: &str = "/etc/sentryusb/networkd-ap";

async fn run(action: &str, variables: &[(&str, &str)], timeout: Duration) -> Result<String> {
    let mut command = tokio::process::Command::new("bash");
    command.args(["-c", SCRIPT, "sentryusb-networkd-ap", action]).envs(variables.iter().copied()).kill_on_drop(true);
    // Credentials travel only in the environment, never in logged arguments.
    let output = tokio::time::timeout(timeout, command.output()).await
        .context("Armbian hotspot operation timed out")?
        .context("Could not run the Armbian hotspot helper")?;
    if !output.status.success() {
        return Err(sentryusb_shell::CommandError {
            code: output.status.code(),
            stderr: sentryusb_shell::clean_stderr(&String::from_utf8_lossy(&output.stderr)),
        }.into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Armbian + networkd + a Netplan WiFi unit, excluding NM installations.
pub async fn applicable() -> Result<bool> {
    // Most devices never spawn a command for this backend.
    if !Path::new("/etc/armbian-release").exists() { return Ok(false); }
    match run("detect", &[], Duration::from_secs(5)).await {
        Ok(_) => Ok(true),
        Err(error) if error.downcast_ref::<sentryusb_shell::CommandError>()
            .is_some_and(|error| error.code == Some(1)) => Ok(false),
        Err(error) => Err(error),
    }
}

pub async fn profile() -> Result<Option<ApProfile>> {
    if !Path::new(DIRECTORY).try_exists()? { return Ok(None); }
    let output = run("info", &[], Duration::from_secs(5)).await?;
    let mut fields = output.lines();
    let ssid = fields.next().context("Armbian AP profile has no SSID")?;
    let ip = fields.next().context("Armbian AP profile has no IP address")?;
    if ssid.is_empty() || ip.parse::<std::net::Ipv4Addr>().is_err() {
        bail!("Armbian AP profile is incomplete");
    }
    Ok(Some(ApProfile { ssid: ssid.into(), ip: ip.into() }))
}

pub async fn configure(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    emitter.progress("Configuring the Armbian Netplan/networkd hotspot");
    crate::apt::apt_install(|message| emitter.progress(message), &["hostapd", "iw"], Duration::from_secs(300)).await?;
    std::fs::create_dir_all("/root/bin")?;
    std::fs::write("/root/bin/networkd-ap.sh", SCRIPT)?;
    let ssid = env.get("AP_SSID", "");
    let pass = env.get("AP_PASS", "");
    let ip = env.get("AP_IP", "192.168.66.1");
    run("configure", &[("AP_SSID", &ssid), ("AP_PASS", &pass), ("AP_IP", &ip)], Duration::from_secs(60)).await?;
    let installed = profile().await?.context("Armbian AP configuration was not installed")?;
    if installed.ssid != ssid || installed.ip != ip {
        bail!("Installed Armbian AP settings do not match the requested SSID and IP");
    }
    emitter.progress("Armbian hotspot configured. Away Mode will activate it when needed.");
    Ok(())
}

pub async fn start() -> Result<()> {
    run("start", &[], Duration::from_secs(120)).await?;
    Ok(())
}

pub async fn stop() -> Result<()> {
    run("stop", &[], Duration::from_secs(60)).await?;
    Ok(())
}

pub async fn check() -> Result<()> {
    run("check", &[], Duration::from_secs(10)).await?;
    Ok(())
}

pub async fn remove() -> Result<()> {
    if Path::new(DIRECTORY).exists() {
        run("remove", &[], Duration::from_secs(60)).await?;
    }
    Ok(())
}

pub fn has_runtime_owner() -> bool {
    Path::new("/etc/armbian-release").exists() && Path::new("/run/sentryusb-networkd-ap-owner").exists()
}

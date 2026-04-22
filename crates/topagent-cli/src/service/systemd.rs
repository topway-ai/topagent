use anyhow::{anyhow, Context, Result};
use std::process::{Command, Output};

use crate::config::defaults::TELEGRAM_SERVICE_UNIT_NAME;

use super::state::ServiceStatusSnapshot;

pub(super) trait SystemdUserManager {
    fn ensure_available(&self) -> Result<()>;
    fn run_checked(&self, args: &[&str], action: &str) -> Result<()>;
    fn format_result(&self, args: &[&str]) -> String;
    fn load_status_snapshot(&self) -> Result<ServiceStatusSnapshot>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RealSystemdUserManager;

impl SystemdUserManager for RealSystemdUserManager {
    fn ensure_available(&self) -> Result<()> {
        let output = run_systemctl_user(&["show-environment"]).map_err(|err| {
            anyhow!(
                "systemd user services are unavailable. `topagent install` currently supports Linux systemd user services only. {}",
                err
            )
        })?;

        if output.status.success() {
            return Ok(());
        }

        Err(anyhow!(
            "systemd user services are unavailable. Make sure `systemctl --user` works in your current Linux session. {}",
            summarize_command_output(&output)
        ))
    }

    fn run_checked(&self, args: &[&str], action: &str) -> Result<()> {
        let output = run_systemctl_user(args)?;
        if output.status.success() {
            return Ok(());
        }

        Err(anyhow!(
            "Failed to {}. {}",
            action,
            summarize_command_output(&output)
        ))
    }

    fn format_result(&self, args: &[&str]) -> String {
        run_systemctl_user(args)
            .map(|output| {
                if output.status.success() {
                    "yes".to_string()
                } else {
                    format!("no ({})", summarize_command_output(&output))
                }
            })
            .unwrap_or_else(|err| format!("no ({})", err))
    }

    fn load_status_snapshot(&self) -> Result<ServiceStatusSnapshot> {
        let output = run_systemctl_user(&[
            "show",
            TELEGRAM_SERVICE_UNIT_NAME,
            "--property=LoadState",
            "--property=UnitFileState",
            "--property=ActiveState",
            "--property=SubState",
            "--property=FragmentPath",
            "--property=Result",
            "--property=ExecMainStatus",
        ])?;
        if !output.status.success() {
            return Err(anyhow!(
                "Failed to inspect the TopAgent Telegram service. {}",
                summarize_command_output(&output)
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_service_status_snapshot(&stdout))
    }
}

pub(super) fn ensure_systemd_user_available() -> Result<()> {
    RealSystemdUserManager.ensure_available()
}

pub(super) fn run_systemctl_user(args: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `systemctl --user {}`", args.join(" ")))
}

pub(super) fn format_systemctl_result(args: &[&str]) -> String {
    RealSystemdUserManager.format_result(args)
}

pub(super) fn run_systemctl_user_checked(args: &[&str], action: &str) -> Result<()> {
    RealSystemdUserManager.run_checked(args, action)
}

fn summarize_command_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("stdout: {}; stderr: {}", stdout, stderr),
        (false, true) => format!("stdout: {}", stdout),
        (true, false) => format!("stderr: {}", stderr),
        (true, true) => format!("exit status {}", output.status),
    }
}

fn parse_service_status_snapshot(stdout: &str) -> ServiceStatusSnapshot {
    let mut snapshot = ServiceStatusSnapshot::default();
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        match key {
            "LoadState" => snapshot.load_state = value,
            "UnitFileState" => snapshot.unit_file_state = value,
            "ActiveState" => snapshot.active_state = value,
            "SubState" => snapshot.sub_state = value,
            "FragmentPath" => snapshot.fragment_path = value,
            "Result" => snapshot.result = value,
            "ExecMainStatus" => snapshot.exec_main_status = value,
            _ => {}
        }
    }
    snapshot
}

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::defaults::{
    CliParams, parse_env_bool, TELEGRAM_SERVICE_UNIT_NAME, TOPAGENT_TOOL_AUTHORING_KEY,
    TOPAGENT_WORKSPACE_KEY,
};
use crate::config::runtime::TelegramModeConfig;
use crate::managed_files::{
    assert_managed_or_absent, ensure_service_setup_present, is_topagent_managed_file,
    read_managed_env_metadata, remove_managed_env_file, remove_managed_file, write_managed_file,
    TOPAGENT_MANAGED_HEADER,
};
use crate::operational_paths::{resolve_config_home, service_paths, ServicePaths};

use super::managed_env::render_service_env_file;
use super::state::{load_control_plane_state, load_service_probe, ServiceStatusSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallRootKind {
    SourceCheckout,
    InstalledBinary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BinaryCleanupOutcome {
    Removed(String),
    Preserved(String),
}

#[derive(Debug, Clone)]
struct InstallRoot {
    kind: InstallRootKind,
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceConfigApplyAction {
    EnabledAndStarted,
    EnabledAndRestarted,
}

impl ServiceConfigApplyAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::EnabledAndStarted => "enabled and started",
            Self::EnabledAndRestarted => "enabled and restarted with updated config",
        }
    }
}

pub(super) fn resolve_current_exe() -> Result<PathBuf> {
    std::env::current_exe()
        .context("cannot determine the TopAgent binary path")?
        .canonicalize()
        .context("cannot resolve the TopAgent binary path")
}

pub(super) fn detect_install_root() -> Result<PathBuf> {
    Ok(detect_install_root_from_exe(&resolve_current_exe()?)?.root)
}

pub(super) fn ensure_systemd_user_available() -> Result<()> {
    let output = run_systemctl_user(&["show-environment"]).map_err(|err| {
        anyhow::anyhow!(
            "systemd user services are unavailable. `topagent install` currently supports Linux systemd user services only. {}",
            err
        )
    })?;

    if output.status.success() {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "systemd user services are unavailable. Make sure `systemctl --user` works in your current Linux session. {}",
        summarize_command_output(&output)
    ))
}

pub(super) fn install_service_with_config(
    config: &TelegramModeConfig,
    paths: &ServicePaths,
) -> Result<ServiceConfigApplyAction> {
    ensure_systemd_user_available()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;
    let was_installed = is_service_installed(paths)?;

    std::fs::create_dir_all(&paths.unit_dir)
        .with_context(|| format!("failed to create {}", paths.unit_dir.display()))?;
    std::fs::create_dir_all(&paths.env_dir)
        .with_context(|| format!("failed to create {}", paths.env_dir.display()))?;

    let current_exe = resolve_current_exe()?;
    let env_contents = render_service_env_file(config)?;
    let unit_contents = render_service_unit_file(&current_exe, config, paths)?;
    write_managed_file(&paths.env_path, &env_contents, true)?;
    write_managed_file(&paths.unit_path, &unit_contents, false)?;

    run_systemctl_user_checked(&["daemon-reload"], "reload the systemd user daemon")?;

    if was_installed {
        run_systemctl_user_checked(
            &["enable", TELEGRAM_SERVICE_UNIT_NAME],
            "enable the TopAgent Telegram service",
        )?;
        run_systemctl_user_checked(
            &["restart", TELEGRAM_SERVICE_UNIT_NAME],
            "restart the TopAgent Telegram service with the updated config",
        )?;
        Ok(ServiceConfigApplyAction::EnabledAndRestarted)
    } else {
        run_systemctl_user_checked(
            &["enable", "--now", TELEGRAM_SERVICE_UNIT_NAME],
            "enable and start the TopAgent Telegram service",
        )?;
        Ok(ServiceConfigApplyAction::EnabledAndStarted)
    }
}

pub(super) fn restart_service_if_installed(paths: &ServicePaths) -> Result<bool> {
    if !is_service_installed(paths)? {
        return Ok(false);
    }

    run_systemctl_user_checked(
        &["restart", TELEGRAM_SERVICE_UNIT_NAME],
        "restart the TopAgent Telegram service",
    )?;
    Ok(true)
}

pub(super) fn is_service_installed(paths: &ServicePaths) -> Result<bool> {
    Ok(load_service_probe(paths).service_installed)
}

pub(crate) fn run_status(params: CliParams) -> Result<()> {
    render_status(params)
}

pub(crate) fn run_uninstall(purge: bool) -> Result<()> {
    uninstall_service_setup(true, purge)
}

pub(crate) fn run_service_status(params: CliParams) -> Result<()> {
    render_status(params)
}

pub(crate) fn run_service_start() -> Result<()> {
    run_service_lifecycle(
        &["start", TELEGRAM_SERVICE_UNIT_NAME],
        "start",
        "started",
        "topagent service stop",
    )
}

pub(crate) fn run_service_stop() -> Result<()> {
    run_service_lifecycle(
        &["stop", TELEGRAM_SERVICE_UNIT_NAME],
        "stop",
        "stopped",
        "topagent service start",
    )
}

pub(crate) fn run_service_restart() -> Result<()> {
    run_service_lifecycle(
        &["restart", TELEGRAM_SERVICE_UNIT_NAME],
        "restart",
        "restarted",
        "topagent status",
    )
}

pub(crate) fn run_service_uninstall(purge: bool) -> Result<()> {
    uninstall_service_setup(false, purge)
}

fn run_service_lifecycle(
    args: &[&str],
    action: &str,
    completed_state: &str,
    next_command: &str,
) -> Result<()> {
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    ensure_service_setup_present(&paths.unit_path, &paths.env_path)?;
    run_systemctl_user_checked(args, &format!("{} the TopAgent Telegram service", action))?;

    println!("TopAgent service {}.", completed_state);
    println!("Service: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Config file: {}", paths.env_path.display());
    println!("Next:");
    println!("  topagent status");
    if next_command.trim() != "topagent status" {
        println!("  {}", next_command);
    }
    println!(
        "  journalctl --user -u {} -n 50 --no-pager",
        TELEGRAM_SERVICE_UNIT_NAME
    );

    Ok(())
}

fn render_status(params: CliParams) -> Result<()> {
    let state = load_control_plane_state(params.model.clone())?;
    let snapshot = state.service_probe.snapshot.as_ref();
    let service_installed = state.service_probe.service_installed;
    let enabled = snapshot
        .as_ref()
        .map(|status| is_enabled_state(status.unit_file_state.as_deref()));
    let active = snapshot
        .as_ref()
        .map(|status| status.active_state.as_deref() == Some("active"));
    let unit_path = state.service_probe.unit_path(&state.paths.unit_path);

    println!("TopAgent status");
    println!("Setup installed: {}", yes_no(state.setup_installed));
    println!("Service installed: {}", yes_no(service_installed));
    if let (Some(enabled), Some(active)) = (enabled, active) {
        println!("Enabled: {}", yes_no(enabled));
        println!("Running: {}", yes_no(active));
    } else {
        println!("Enabled: unknown");
        println!("Running: unknown");
    }
    println!("Service: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Config file: {}", state.paths.env_path.display());
    println!("Unit file: {}", unit_path.display());

    if let Some(workspace) = state.workspace() {
        println!("Workspace: {}", workspace);
    }
    println!(
        "Configured default model: {} ({})",
        state.model_selection.configured_default.model_id,
        state.model_selection.configured_default.source.label()
    );
    println!(
        "Effective model: {} ({})",
        state.model_selection.effective.model_id,
        state.model_selection.effective.source.label()
    );
    if let Some(enabled) = parse_env_bool(
        state
            .env_values
            .get(TOPAGENT_TOOL_AUTHORING_KEY)
            .map(String::as_str),
    ) {
        println!("Tool authoring: {}", if enabled { "on" } else { "off" });
    }

    if service_installed {
        if let Some(status) = &snapshot {
            if let Some(active_state) = &status.active_state {
                let sub_state = status.sub_state.as_deref().unwrap_or("unknown");
                println!("Last state: {} ({})", active_state, sub_state);
            }
            if active != Some(true) {
                if let Some(result) = &status.result {
                    if result != "success" {
                        println!("Hint: service last result was {}", result);
                    }
                }
                if let Some(exit_status) = &status.exec_main_status {
                    if exit_status != "0" {
                        println!("Exit status: {}", exit_status);
                    }
                }
                println!(
                    "Inspect logs: journalctl --user -u {} -n 50 --no-pager",
                    TELEGRAM_SERVICE_UNIT_NAME
                );
            }
        }
    } else if !state.setup_installed {
        println!("Hint: run `topagent install` to configure the Telegram background service.");
    } else if let Some(status) = &snapshot {
        if let Some(active_state) = &status.active_state {
            let sub_state = status.sub_state.as_deref().unwrap_or("unknown");
            println!("Last state: {} ({})", active_state, sub_state);
        }
    } else if let Some(err) = &state.service_probe.snapshot_error {
        println!("Hint: {}", err);
    } else if let Some(err) = &state.service_probe.systemd_error {
        println!("Hint: {}", err);
    } else {
        println!("Hint: run `topagent install` to configure the Telegram background service.");
    }

    Ok(())
}

fn uninstall_service_setup(remove_binary: bool, purge: bool) -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let managed_unit = paths.unit_path.exists() && is_topagent_managed_file(&paths.unit_path)?;
    let managed_env = paths.env_path.exists() && is_topagent_managed_file(&paths.env_path)?;
    let should_manage_service = managed_unit || managed_env;
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let mut stopped = String::from("not attempted");
    let mut disabled = String::from("not attempted");

    if should_manage_service && systemd_available.is_ok() {
        stopped = format_systemctl_result(&["stop", TELEGRAM_SERVICE_UNIT_NAME]);
        disabled = format_systemctl_result(&["disable", TELEGRAM_SERVICE_UNIT_NAME]);
    } else if should_manage_service {
        if let Err(err) = &systemd_available {
            let note = format!("not attempted ({})", err);
            stopped = note.clone();
            disabled = note;
        }
    } else if let Err(err) = &systemd_available {
        let note = format!("no managed service found ({})", err);
        stopped = note.clone();
        disabled = note;
    } else {
        stopped = "no managed service found".to_string();
        disabled = "no managed service found".to_string();
    }

    let mut removed = Vec::new();
    let mut preserved = Vec::new();

    match remove_managed_file(&paths.unit_path, "unit file")? {
        Some(path) => removed.push(path),
        None => {
            if paths.unit_path.exists() {
                preserved.push(format!(
                    "unit file left in place (not managed by TopAgent): {}",
                    paths.unit_path.display()
                ));
            }
        }
    }

    match remove_managed_env_file(&paths.env_path)? {
        Some(path) => removed.push(path),
        None => {
            if paths.env_path.exists() {
                preserved.push(format!(
                    "env file left in place (not managed by TopAgent): {}",
                    paths.env_path.display()
                ));
            }
        }
    }

    let workspace_path = env_values.get(TOPAGENT_WORKSPACE_KEY);

    if purge {
        if let Some(workspace) = workspace_path {
            let ws_path = PathBuf::from(workspace);
            let topagent_dir = ws_path.join(".topagent");
            if topagent_dir.exists() && topagent_dir.is_dir() {
                if let Err(err) = std::fs::remove_dir_all(&topagent_dir) {
                    preserved.push(format!(
                        ".topagent/ left in place (could not remove {}: {})",
                        topagent_dir.display(),
                        err
                    ));
                } else {
                    removed.push(format!("workspace .topagent/ {}", topagent_dir.display()));
                }
            }

            let cache_dir = if let Ok(config_home) = resolve_config_home() {
                config_home.join("topagent").join("cache")
            } else {
                ws_path.join(".topagent").join("cache")
            };
            if cache_dir.exists() && cache_dir.is_dir() {
                if let Err(err) = std::fs::remove_dir_all(&cache_dir) {
                    preserved.push(format!(
                        "cache/ left in place (could not remove {}: {})",
                        cache_dir.display(),
                        err
                    ));
                } else {
                    removed.push(format!("cache directory {}", cache_dir.display()));
                }
            }
        }
    } else if let Some(workspace) = workspace_path {
        preserved.push(format!("workspace directory preserved: {}", workspace));
    }

    if remove_binary {
        match cleanup_current_binary_for_uninstall() {
            BinaryCleanupOutcome::Removed(item) => removed.push(item),
            BinaryCleanupOutcome::Preserved(item) => preserved.push(item),
        }
    }

    let mut daemon_reload = String::from("not needed");
    if should_manage_service && systemd_available.is_ok() {
        daemon_reload = format_systemctl_result(&["daemon-reload"]);
    }

    println!("TopAgent uninstall");
    println!("Stopped: {}", stopped);
    println!("Disabled: {}", disabled);
    println!("Daemon reload: {}", daemon_reload);
    if purge {
        println!("Mode: purge");
    } else {
        println!("Mode: standard");
    }
    println!("Removed:");
    if removed.is_empty() {
        println!("  nothing");
    } else {
        for item in &removed {
            println!("  {}", item);
        }
    }
    println!("Left in place:");
    if preserved.is_empty() {
        println!("  nothing");
    } else {
        for item in &preserved {
            println!("  {}", item);
        }
    }

    Ok(())
}

fn cleanup_current_binary_for_uninstall() -> BinaryCleanupOutcome {
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (could not determine current binary path: {})",
                err
            ));
        }
    };

    cleanup_binary_for_uninstall_at_path(&current_exe)
}

fn cleanup_binary_for_uninstall_at_path(exe: &Path) -> BinaryCleanupOutcome {
    let resolved_exe = match exe.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (could not resolve {}: {})",
                exe.display(),
                err
            ));
        }
    };

    match detect_install_root_from_exe(&resolved_exe) {
        Ok(InstallRoot {
            kind: InstallRootKind::SourceCheckout,
            ..
        }) => BinaryCleanupOutcome::Preserved(format!(
            "source checkout binary preserved: {}",
            resolved_exe.display()
        )),
        Ok(InstallRoot {
            kind: InstallRootKind::InstalledBinary,
            ..
        }) => match std::fs::remove_file(&resolved_exe) {
            Ok(()) => BinaryCleanupOutcome::Removed(format!(
                "installed binary {}",
                resolved_exe.display()
            )),
            Err(err) => BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (failed to remove {}: {})",
                resolved_exe.display(),
                err
            )),
        },
        Err(err) => BinaryCleanupOutcome::Preserved(format!(
            "installed binary left in place (could not classify {}: {})",
            resolved_exe.display(),
            err
        )),
    }
}

fn detect_install_root_from_exe(exe: &Path) -> Result<InstallRoot> {
    if let Some(target_dir) = exe
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "target"))
    {
        let repo_root = target_dir.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "TopAgent is running from a target directory, but the repo root could not be determined."
            )
        })?;
        if looks_like_source_checkout(repo_root) {
            return Ok(InstallRoot {
                kind: InstallRootKind::SourceCheckout,
                root: repo_root.to_path_buf(),
            });
        }
        return Err(anyhow::anyhow!(
            "TopAgent is running from a target directory, but this does not look like a TopAgent source checkout. Re-run from the repo root or install the binary into a stable directory before `topagent install`."
        ));
    }

    let install_dir = exe.parent().ok_or_else(|| {
        anyhow::anyhow!("Could not determine the directory that contains the TopAgent binary.")
    })?;
    Ok(InstallRoot {
        kind: InstallRootKind::InstalledBinary,
        root: install_dir.to_path_buf(),
    })
}

fn looks_like_source_checkout(root: &Path) -> bool {
    root.join("Cargo.toml").is_file()
        && root
            .join("crates")
            .join("topagent-cli")
            .join("Cargo.toml")
            .is_file()
        && root
            .join("crates")
            .join("topagent-core")
            .join("Cargo.toml")
            .is_file()
}

fn render_service_unit_file(
    current_exe: &Path,
    config: &TelegramModeConfig,
    paths: &ServicePaths,
) -> Result<String> {
    let exec_start = render_service_exec_start(current_exe);
    let workspace = config.workspace.display().to_string();
    let env_path = paths.env_path.display().to_string();
    Ok(format!(
        "{header}
[Unit]
Description=TopAgent Telegram bot
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
WorkingDirectory={working_directory}
EnvironmentFile={env_file}
ExecStart={exec_start}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
",
        header = TOPAGENT_MANAGED_HEADER,
        working_directory = escape_systemd_value(&workspace),
        env_file = escape_systemd_value(&env_path),
        exec_start = exec_start,
    ))
}

fn render_service_exec_start(current_exe: &Path) -> String {
    let mut args = [current_exe.display().to_string(), "telegram".to_string()];
    args.iter_mut().for_each(|arg| {
        if arg.contains('\n') {
            *arg = arg.replace('\n', " ");
        }
    });
    args.iter()
        .map(|arg| escape_systemd_value(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape_systemd_value(value: &str) -> String {
    let safe = !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@')
        });
    if safe {
        return value.to_string();
    }

    let mut escaped = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn run_systemctl_user(args: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `systemctl --user {}`", args.join(" ")))
}

fn format_systemctl_result(args: &[&str]) -> String {
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

fn run_systemctl_user_checked(args: &[&str], action: &str) -> Result<()> {
    let output = run_systemctl_user(args)?;
    if output.status.success() {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "Failed to {}. {}",
        action,
        summarize_command_output(&output)
    ))
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

pub(super) fn load_service_status_snapshot() -> Result<ServiceStatusSnapshot> {
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
        return Err(anyhow::anyhow!(
            "Failed to inspect the TopAgent Telegram service. {}",
            summarize_command_output(&output)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_service_status_snapshot(&stdout))
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

fn is_enabled_state(state: Option<&str>) -> bool {
    matches!(state, Some("enabled" | "enabled-runtime" | "linked"))
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_install_root_uses_repo_root_for_source_checkout_binary() {
        let repo = TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-cli")).unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-core")).unwrap();
        std::fs::create_dir_all(repo.path().join("target").join("debug")).unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-cli")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-cli\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-core")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-core\"\n",
        )
        .unwrap();
        let exe = repo.path().join("target").join("debug").join("topagent");
        std::fs::write(&exe, "").unwrap();

        let detected = detect_install_root_from_exe(&exe).unwrap();

        assert_eq!(detected.kind, InstallRootKind::SourceCheckout);
        assert_eq!(detected.root, repo.path());
    }

    #[test]
    fn test_detect_install_root_uses_binary_directory_for_installed_binary() {
        let install_dir = TempDir::new().unwrap();
        let exe = install_dir.path().join("topagent");
        std::fs::write(&exe, "").unwrap();

        let detected = detect_install_root_from_exe(&exe).unwrap();

        assert_eq!(detected.kind, InstallRootKind::InstalledBinary);
        assert_eq!(detected.root, install_dir.path());
    }

    #[test]
    fn test_detect_install_root_rejects_ambiguous_target_layout() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("target").join("debug")).unwrap();
        let exe = temp.path().join("target").join("debug").join("topagent");
        std::fs::write(&exe, "").unwrap();

        let err = detect_install_root_from_exe(&exe).unwrap_err().to_string();

        assert!(err.contains("does not look like a TopAgent source checkout"));
    }

    #[test]
    fn test_cleanup_binary_for_uninstall_removes_installed_binary() {
        let install_dir = TempDir::new().unwrap();
        let exe = install_dir.path().join("topagent");
        std::fs::write(&exe, "").unwrap();
        let canonical_exe = exe.canonicalize().unwrap();

        let outcome = cleanup_binary_for_uninstall_at_path(&exe);

        assert_eq!(
            outcome,
            BinaryCleanupOutcome::Removed(format!("installed binary {}", canonical_exe.display()))
        );
        assert!(!exe.exists());
    }

    #[test]
    fn test_cleanup_binary_for_uninstall_preserves_source_checkout_binary() {
        let repo = TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-cli")).unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-core")).unwrap();
        std::fs::create_dir_all(repo.path().join("target").join("debug")).unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-cli")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-cli\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-core")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-core\"\n",
        )
        .unwrap();
        let exe = repo.path().join("target").join("debug").join("topagent");
        std::fs::write(&exe, "").unwrap();

        let outcome = cleanup_binary_for_uninstall_at_path(&exe);

        assert_eq!(
            outcome,
            BinaryCleanupOutcome::Preserved(format!(
                "source checkout binary preserved: {}",
                exe.canonicalize().unwrap().display()
            ))
        );
        assert!(exe.exists());
    }
}

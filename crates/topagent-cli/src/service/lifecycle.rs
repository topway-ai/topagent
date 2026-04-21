use anyhow::{Context, Result};

use crate::commands::surface::{LIFECYCLE_LANES, PRODUCT_NAME};
use crate::config::defaults::{
    parse_env_bool, CliParams, TELEGRAM_SERVICE_UNIT_NAME, TOPAGENT_TOOL_AUTHORING_KEY,
};
use crate::config::runtime::TelegramModeConfig;
use crate::managed_files::{
    assert_managed_or_absent, ensure_service_setup_present, write_managed_file,
};
use crate::operational_paths::{service_paths, ServicePaths};

use super::detect::resolve_current_exe;
use super::managed_env::render_service_env_file;
use super::state::{load_control_plane_state, load_service_probe};
use super::systemd::{ensure_systemd_user_available, run_systemctl_user_checked};
use super::uninstall::uninstall_service_setup;
use super::unit::render_service_unit_file;

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
            &format!("enable the {PRODUCT_NAME} Telegram service"),
        )?;
        run_systemctl_user_checked(
            &["restart", TELEGRAM_SERVICE_UNIT_NAME],
            &format!("restart the {PRODUCT_NAME} Telegram service with the updated config"),
        )?;
        Ok(ServiceConfigApplyAction::EnabledAndRestarted)
    } else {
        run_systemctl_user_checked(
            &["enable", "--now", TELEGRAM_SERVICE_UNIT_NAME],
            &format!("enable and start the {PRODUCT_NAME} Telegram service"),
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
        &format!("restart the {PRODUCT_NAME} Telegram service"),
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
    run_systemctl_user_checked(
        args,
        &format!("{} the {PRODUCT_NAME} Telegram service", action),
    )?;

    println!("{PRODUCT_NAME} service {}.", completed_state);
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

    println!("{PRODUCT_NAME} status");
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
    println!("Lifecycle sources of truth:");
    for lane in LIFECYCLE_LANES {
        println!(
            "  {}: {} ({})",
            lane.name, lane.source_of_truth_command, lane.owns
        );
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

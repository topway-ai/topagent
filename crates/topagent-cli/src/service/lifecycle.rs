use anyhow::{Context, Result};
use std::path::Path;

use crate::commands::surface::{LIFECYCLE_LANES, PRODUCT_NAME};
use crate::config::defaults::{CliParams, TELEGRAM_SERVICE_UNIT_NAME};
use crate::config::runtime::TelegramModeConfig;
use crate::managed_files::{
    assert_managed_or_absent, ensure_service_install_present, write_managed_file,
};
use crate::operational_paths::{service_paths, ServicePaths};

use super::detect::resolve_current_exe;
use super::managed_env::render_service_env_file;
use super::state::{load_control_plane_state, load_service_probe, load_service_probe_with_systemd};
use super::systemd::{run_systemctl_user_checked, RealSystemdUserManager, SystemdUserManager};
use super::uninstall::uninstall_installation;
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
    let systemd = RealSystemdUserManager;
    systemd.ensure_available()?;
    let current_exe = resolve_current_exe()?;
    install_service_with_config_after_availability(config, paths, &systemd, &current_exe)
}

#[cfg(test)]
fn install_service_with_config_using(
    config: &TelegramModeConfig,
    paths: &ServicePaths,
    systemd: &dyn SystemdUserManager,
    current_exe: &Path,
) -> Result<ServiceConfigApplyAction> {
    systemd.ensure_available()?;
    install_service_with_config_after_availability(config, paths, systemd, current_exe)
}

fn install_service_with_config_after_availability(
    config: &TelegramModeConfig,
    paths: &ServicePaths,
    systemd: &dyn SystemdUserManager,
    current_exe: &Path,
) -> Result<ServiceConfigApplyAction> {
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;
    let was_installed = is_service_installed_using(paths, systemd)?;

    std::fs::create_dir_all(&paths.unit_dir)
        .with_context(|| format!("failed to create {}", paths.unit_dir.display()))?;
    std::fs::create_dir_all(&paths.env_dir)
        .with_context(|| format!("failed to create {}", paths.env_dir.display()))?;

    let env_contents = render_service_env_file(config)?;
    let unit_contents = render_service_unit_file(current_exe, config, paths)?;
    write_managed_file(&paths.env_path, &env_contents, true)?;
    write_managed_file(&paths.unit_path, &unit_contents, false)?;

    systemd.run_checked(&["daemon-reload"], "reload the systemd user daemon")?;

    if was_installed {
        systemd.run_checked(
            &["enable", TELEGRAM_SERVICE_UNIT_NAME],
            &format!("enable the {PRODUCT_NAME} Telegram service"),
        )?;
        systemd.run_checked(
            &["restart", TELEGRAM_SERVICE_UNIT_NAME],
            &format!("restart the {PRODUCT_NAME} Telegram service with the updated config"),
        )?;
        Ok(ServiceConfigApplyAction::EnabledAndRestarted)
    } else {
        systemd.run_checked(
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

fn is_service_installed_using(
    paths: &ServicePaths,
    systemd: &dyn SystemdUserManager,
) -> Result<bool> {
    Ok(load_service_probe_with_systemd(paths, systemd).service_installed)
}

pub(crate) fn run_status(params: CliParams) -> Result<()> {
    render_status(params)
}

pub(crate) fn run_uninstall(purge: bool) -> Result<()> {
    uninstall_installation(true, purge)
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
    uninstall_installation(false, purge)
}

fn run_service_lifecycle(
    args: &[&str],
    action: &str,
    completed_state: &str,
    next_command: &str,
) -> Result<()> {
    let paths = service_paths()?;
    apply_service_lifecycle_using(&paths, &RealSystemdUserManager, args, action)?;

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

fn apply_service_lifecycle_using(
    paths: &ServicePaths,
    systemd: &dyn SystemdUserManager,
    args: &[&str],
    action: &str,
) -> Result<()> {
    systemd.ensure_available()?;
    ensure_service_install_present(&paths.unit_path, &paths.env_path)?;
    systemd.run_checked(
        args,
        &format!("{} the {PRODUCT_NAME} Telegram service", action),
    )
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
    println!(
        "Installation present: {}",
        yes_no(state.installation_present)
    );
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
    } else if !state.installation_present {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model_selection::SelectedProvider;
    use crate::managed_files::{write_managed_file, TOPAGENT_MANAGED_HEADER};
    use crate::service::state::ServiceStatusSnapshot;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use topagent_core::{ModelRoute, ProviderKind, RuntimeOptions};

    #[derive(Debug)]
    struct FakeSystemd {
        snapshot: RefCell<Result<ServiceStatusSnapshot, String>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeSystemd {
        fn with_load_state(load_state: &str) -> Self {
            Self {
                snapshot: RefCell::new(Ok(ServiceStatusSnapshot {
                    load_state: Some(load_state.to_string()),
                    unit_file_state: Some("enabled".to_string()),
                    active_state: Some("active".to_string()),
                    sub_state: Some("running".to_string()),
                    fragment_path: None,
                    result: Some("success".to_string()),
                    exec_main_status: Some("0".to_string()),
                })),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl SystemdUserManager for FakeSystemd {
        fn ensure_available(&self) -> Result<()> {
            Ok(())
        }

        fn run_checked(&self, args: &[&str], _action: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            Ok(())
        }

        fn format_result(&self, args: &[&str]) -> String {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            "yes".to_string()
        }

        fn load_status_snapshot(&self) -> Result<ServiceStatusSnapshot> {
            self.snapshot.borrow().clone().map_err(anyhow::Error::msg)
        }
    }

    fn test_paths(root: &Path) -> ServicePaths {
        ServicePaths {
            unit_dir: root.join("systemd/user"),
            unit_path: root.join("systemd/user").join(TELEGRAM_SERVICE_UNIT_NAME),
            env_dir: root.join("topagent/services"),
            env_path: root.join("topagent/services/topagent-telegram.env"),
        }
    }

    fn test_config(workspace: PathBuf) -> TelegramModeConfig {
        TelegramModeConfig {
            token: "123456:telegram-token".to_string(),
            openrouter_api_key: Some("openrouter-key".to_string()),
            opencode_api_key: None,
            configured_default_model: "openai/gpt-5.4".to_string(),
            route: ModelRoute::new(ProviderKind::OpenRouter, "openai/gpt-5.4"),
            workspace,
            options: RuntimeOptions::default(),
            selected_provider: SelectedProvider::OpenRouter,
            allowed_dm_username: Some("operator".to_string()),
            bound_dm_user_id: Some(424242),
        }
    }

    #[test]
    fn test_install_service_with_fake_systemd_writes_config_and_starts_new_service() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(temp.path());
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let config = test_config(workspace.clone());
        let systemd = FakeSystemd::with_load_state("not-found");

        let action = install_service_with_config_using(
            &config,
            &paths,
            &systemd,
            Path::new("/opt/topagent/bin/topagent"),
        )
        .unwrap();

        assert_eq!(action, ServiceConfigApplyAction::EnabledAndStarted);
        assert_eq!(
            systemd.calls(),
            vec![
                vec!["daemon-reload".to_string()],
                vec![
                    "enable".to_string(),
                    "--now".to_string(),
                    TELEGRAM_SERVICE_UNIT_NAME.to_string()
                ],
            ]
        );
        let env = std::fs::read_to_string(&paths.env_path).unwrap();
        assert!(env.contains("TOPAGENT_SERVICE_MANAGED=1"));
        assert!(env.contains("TOPAGENT_PROVIDER=\"OpenRouter\""));
        assert!(env.contains("TOPAGENT_MODEL=\"openai/gpt-5.4\""));
        assert!(env.contains("TELEGRAM_ALLOWED_DM_USERNAME=\"operator\""));
        assert!(env.contains("TELEGRAM_BOUND_DM_USER_ID=\"424242\""));

        let unit = std::fs::read_to_string(&paths.unit_path).unwrap();
        assert!(unit.contains("ExecStart=/opt/topagent/bin/topagent telegram"));
        assert!(unit.contains(&format!("WorkingDirectory={}", workspace.display())));
        assert!(unit.contains(&format!("EnvironmentFile={}", paths.env_path.display())));
    }

    #[test]
    fn test_reinstall_with_fake_systemd_restarts_existing_service() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(temp.path());
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&paths.unit_dir).unwrap();
        std::fs::create_dir_all(&paths.env_dir).unwrap();
        write_managed_file(
            &paths.unit_path,
            &format!("{TOPAGENT_MANAGED_HEADER}\n[Unit]\n"),
            false,
        )
        .unwrap();
        write_managed_file(
            &paths.env_path,
            &format!("{TOPAGENT_MANAGED_HEADER}\nTOPAGENT_SERVICE_MANAGED=1\n"),
            true,
        )
        .unwrap();
        let config = test_config(workspace);
        let systemd = FakeSystemd::with_load_state("loaded");

        let action = install_service_with_config_using(
            &config,
            &paths,
            &systemd,
            Path::new("/opt/topagent/bin/topagent"),
        )
        .unwrap();

        assert_eq!(action, ServiceConfigApplyAction::EnabledAndRestarted);
        assert_eq!(
            systemd.calls(),
            vec![
                vec!["daemon-reload".to_string()],
                vec!["enable".to_string(), TELEGRAM_SERVICE_UNIT_NAME.to_string()],
                vec![
                    "restart".to_string(),
                    TELEGRAM_SERVICE_UNIT_NAME.to_string()
                ],
            ]
        );
    }

    #[test]
    fn test_service_lifecycle_with_fake_systemd_requires_managed_install_and_runs_command() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(temp.path());
        std::fs::create_dir_all(&paths.unit_dir).unwrap();
        std::fs::create_dir_all(&paths.env_dir).unwrap();
        write_managed_file(
            &paths.unit_path,
            &format!("{TOPAGENT_MANAGED_HEADER}\n[Unit]\n"),
            false,
        )
        .unwrap();
        write_managed_file(
            &paths.env_path,
            &format!("{TOPAGENT_MANAGED_HEADER}\nTOPAGENT_SERVICE_MANAGED=1\n"),
            true,
        )
        .unwrap();
        let systemd = FakeSystemd::with_load_state("loaded");

        apply_service_lifecycle_using(
            &paths,
            &systemd,
            &["start", TELEGRAM_SERVICE_UNIT_NAME],
            "start",
        )
        .unwrap();

        assert_eq!(
            systemd.calls(),
            vec![vec![
                "start".to_string(),
                TELEGRAM_SERVICE_UNIT_NAME.to_string()
            ]]
        );
    }
}

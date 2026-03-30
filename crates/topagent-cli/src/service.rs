use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::*;
use crate::managed_files::*;

#[derive(Debug, Clone)]
pub(crate) struct ServicePaths {
    pub unit_dir: PathBuf,
    pub unit_path: PathBuf,
    pub env_dir: PathBuf,
    pub env_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ServiceStatusSnapshot {
    load_state: Option<String>,
    unit_file_state: Option<String>,
    active_state: Option<String>,
    sub_state: Option<String>,
    fragment_path: Option<String>,
    result: Option<String>,
    exec_main_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallRootKind {
    SourceCheckout,
    InstalledBinary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BinaryCleanupOutcome {
    Removed(String),
    Preserved(String),
}

#[derive(Debug, Clone)]
struct InstallRoot {
    #[allow(dead_code)]
    kind: InstallRootKind,
    root: PathBuf,
}

// ── Service command dispatch ──

pub(crate) fn run_service_command(
    command: crate::ServiceCommands,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    match command {
        crate::ServiceCommands::Install { token } => run_service_install(
            token,
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
        ),
        crate::ServiceCommands::Status => run_service_status(),
        crate::ServiceCommands::Start => run_service_start(),
        crate::ServiceCommands::Stop => run_service_stop(),
        crate::ServiceCommands::Restart => run_service_restart(),
        crate::ServiceCommands::Uninstall => run_service_uninstall(),
    }
}

// ── Install ──

pub(crate) fn run_install(
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;
    let existing_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let workspace = resolve_install_workspace_path(workspace, &existing_values)?;

    println!("TopAgent setup");
    println!("This will configure and start your Telegram background service.");
    println!();

    let api_key = prompt_for_install_value(
        "OpenRouter API key",
        api_key.as_deref().or_else(|| {
            existing_values
                .get(OPENROUTER_API_KEY_KEY)
                .map(String::as_str)
        }),
        require_openrouter_api_key,
    )?;
    let token = prompt_for_install_value(
        "Telegram bot token",
        existing_values
            .get(TELEGRAM_BOT_TOKEN_KEY)
            .map(String::as_str),
        require_telegram_token,
    )?;

    let config = TelegramModeConfig {
        token,
        api_key,
        route: build_route(provider, model)?,
        workspace,
        options: build_runtime_options(max_steps, max_retries, timeout_secs),
    };
    install_service_with_config(&config, &paths)?;

    println!();
    print_service_installed(
        "TopAgent installed.",
        &paths.env_path,
        Some(&config.workspace),
    );

    Ok(())
}

pub(crate) fn run_status() -> Result<()> {
    render_status()
}

pub(crate) fn run_uninstall() -> Result<()> {
    uninstall_service_setup(true)
}

pub(crate) fn print_service_installed(
    headline: &str,
    env_path: &Path,
    workspace: Option<&PathBuf>,
) {
    println!("{}", headline);
    println!("Service: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Started: yes");
    println!("Config file: {}", env_path.display());
    if let Some(ws) = workspace {
        println!("Workspace: {}", ws.display());
    }
    println!("Inspect:");
    println!("  topagent status");
    println!("  systemctl --user status {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("  journalctl --user -u {} -f", TELEGRAM_SERVICE_UNIT_NAME);
}

// ── Install helpers ──

fn resolve_install_workspace_path(
    workspace: Option<PathBuf>,
    existing_values: &HashMap<String, String>,
) -> Result<PathBuf> {
    let target = if let Some(workspace) = workspace {
        workspace
    } else if let Some(existing_workspace) = existing_values.get(TOPAGENT_WORKSPACE_KEY) {
        PathBuf::from(existing_workspace)
    } else {
        detect_install_root()?.root.join("workspace")
    };
    ensure_directory(target)
}

fn resolve_current_exe() -> Result<PathBuf> {
    std::env::current_exe()
        .context("cannot determine the TopAgent binary path")?
        .canonicalize()
        .context("cannot resolve the TopAgent binary path")
}

fn detect_install_root() -> Result<InstallRoot> {
    detect_install_root_from_exe(&resolve_current_exe()?)
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

fn ensure_directory(path: PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    path.canonicalize()
        .with_context(|| format!("failed to access {}", path.display()))
}

fn prompt_for_install_value(
    label: &str,
    existing_value: Option<&str>,
    validator: fn(Option<String>) -> Result<String>,
) -> Result<String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();

    loop {
        if existing_value.is_some() {
            print!("{label} [press Enter to keep the current value]: ");
        } else {
            print!("{label}: ");
        }
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .context("failed to read installer input")?;
        if read == 0 {
            return Err(anyhow::anyhow!(
                "Installer input ended unexpectedly. Re-run `topagent install` in an interactive shell."
            ));
        }

        let candidate = line.trim();
        let value = if candidate.is_empty() {
            existing_value.map(str::to_string)
        } else {
            Some(candidate.to_string())
        };

        match validator(value) {
            Ok(value) => return Ok(value),
            Err(err) => {
                println!("{}", err);
            }
        }
    }
}

fn resolve_config_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine your config directory. Set XDG_CONFIG_HOME or HOME first."
            )
        })?;
    Ok(home.join(".config"))
}

pub(crate) fn service_paths() -> Result<ServicePaths> {
    let config_home = resolve_config_home()?;
    Ok(ServicePaths {
        unit_dir: config_home.join("systemd").join("user"),
        unit_path: config_home
            .join("systemd")
            .join("user")
            .join(TELEGRAM_SERVICE_UNIT_NAME),
        env_dir: config_home.join("topagent").join("services"),
        env_path: config_home
            .join("topagent")
            .join("services")
            .join("topagent-telegram.env"),
    })
}

// ── Service lifecycle ──

fn run_service_install(
    token: Option<String>,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let config = resolve_telegram_mode_config(
        token,
        api_key,
        provider,
        model,
        workspace,
        max_steps,
        max_retries,
        timeout_secs,
    )?;
    let paths = service_paths()?;
    install_service_with_config(&config, &paths)?;
    print_service_installed(
        "TopAgent service installed.",
        &paths.env_path,
        Some(&config.workspace),
    );
    Ok(())
}

fn run_service_status() -> Result<()> {
    render_status()
}

fn run_service_start() -> Result<()> {
    run_service_lifecycle(
        &["start", TELEGRAM_SERVICE_UNIT_NAME],
        "start",
        "started",
        "topagent service stop",
    )
}

fn run_service_stop() -> Result<()> {
    run_service_lifecycle(
        &["stop", TELEGRAM_SERVICE_UNIT_NAME],
        "stop",
        "stopped",
        "topagent service start",
    )
}

fn run_service_restart() -> Result<()> {
    run_service_lifecycle(
        &["restart", TELEGRAM_SERVICE_UNIT_NAME],
        "restart",
        "restarted",
        "topagent status",
    )
}

fn run_service_uninstall() -> Result<()> {
    uninstall_service_setup(false)
}

fn run_service_lifecycle(
    args: &[&str],
    action: &str,
    completed_state: &str,
    next_command: &str,
) -> Result<()> {
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    ensure_service_setup_present(&paths)?;
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

fn install_service_with_config(config: &TelegramModeConfig, paths: &ServicePaths) -> Result<()> {
    ensure_systemd_user_available()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;

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
    run_systemctl_user_checked(
        &["enable", "--now", TELEGRAM_SERVICE_UNIT_NAME],
        "enable and start the TopAgent Telegram service",
    )?;

    Ok(())
}

// ── Status ──

fn render_status() -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let config_installed = paths.env_path.exists() && is_topagent_managed_file(&paths.env_path)?;
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let snapshot_result = if systemd_available.is_ok() {
        Some(load_service_status_snapshot())
    } else {
        None
    };
    let snapshot = snapshot_result
        .as_ref()
        .and_then(|result| result.as_ref().ok());
    let service_installed = snapshot
        .as_ref()
        .and_then(|status| status.load_state.as_deref())
        .map(|state| state != "not-found")
        .unwrap_or(paths.unit_path.exists());
    let setup_installed = config_installed || service_installed;
    let enabled = snapshot
        .as_ref()
        .map(|status| is_enabled_state(status.unit_file_state.as_deref()));
    let active = snapshot
        .as_ref()
        .map(|status| status.active_state.as_deref() == Some("active"));
    let unit_path = snapshot
        .as_ref()
        .and_then(|status| status.fragment_path.as_ref())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.unit_path.clone());

    println!("TopAgent status");
    println!("Setup installed: {}", yes_no(setup_installed));
    println!("Service installed: {}", yes_no(service_installed));
    if let (Some(enabled), Some(active)) = (enabled, active) {
        println!("Enabled: {}", yes_no(enabled));
        println!("Running: {}", yes_no(active));
    } else {
        println!("Enabled: unknown");
        println!("Running: unknown");
    }
    println!("Service: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Config file: {}", paths.env_path.display());
    println!("Unit file: {}", unit_path.display());

    if let Some(workspace) = env_values.get(TOPAGENT_WORKSPACE_KEY) {
        println!("Workspace: {}", workspace);
    }
    if let Some(provider) = env_values.get(TOPAGENT_PROVIDER_KEY) {
        let model = env_values
            .get(TOPAGENT_MODEL_KEY)
            .map(String::as_str)
            .unwrap_or("(default)");
        println!("Route: {} | {}", provider, model);
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
    } else if !setup_installed {
        println!("Hint: run `topagent install` to configure the Telegram background service.");
    } else if let Some(status) = &snapshot {
        if let Some(active_state) = &status.active_state {
            let sub_state = status.sub_state.as_deref().unwrap_or("unknown");
            println!("Last state: {} ({})", active_state, sub_state);
        }
    } else if let Some(Err(err)) = snapshot_result {
        println!("Hint: {}", err);
    } else if let Err(err) = systemd_available {
        println!("Hint: {}", err);
    } else {
        println!("Hint: run `topagent install` to configure the Telegram background service.");
    }

    Ok(())
}

// ── Uninstall ──

fn uninstall_service_setup(remove_binary: bool) -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let managed_unit = paths.unit_path.exists() && is_topagent_managed_file(&paths.unit_path)?;
    let managed_env = paths.env_path.exists() && is_topagent_managed_file(&paths.env_path)?;
    let should_manage_service = managed_unit || managed_env;
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let mut stopped = String::from("not attempted");
    let mut disabled = String::from("not attempted");

    if should_manage_service && systemd_available.is_ok() {
        stopped = run_systemctl_user(&["stop", TELEGRAM_SERVICE_UNIT_NAME])
            .map(|output| {
                if output.status.success() {
                    "yes".to_string()
                } else {
                    format!("no ({})", summarize_command_output(&output))
                }
            })
            .unwrap_or_else(|err| format!("no ({})", err));
        disabled = run_systemctl_user(&["disable", TELEGRAM_SERVICE_UNIT_NAME])
            .map(|output| {
                if output.status.success() {
                    "yes".to_string()
                } else {
                    format!("no ({})", summarize_command_output(&output))
                }
            })
            .unwrap_or_else(|err| format!("no ({})", err));
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

    if let Some(workspace) = env_values.get(TOPAGENT_WORKSPACE_KEY) {
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
        daemon_reload = run_systemctl_user(&["daemon-reload"])
            .map(|output| {
                if output.status.success() {
                    "yes".to_string()
                } else {
                    format!("no ({})", summarize_command_output(&output))
                }
            })
            .unwrap_or_else(|err| format!("no ({})", err));
    }

    println!("TopAgent uninstall");
    println!("Stopped: {}", stopped);
    println!("Disabled: {}", disabled);
    println!("Daemon reload: {}", daemon_reload);
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
            ))
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
            ))
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

// ── Service file rendering ──

fn render_service_unit_file(
    current_exe: &Path,
    config: &TelegramModeConfig,
    paths: &ServicePaths,
) -> Result<String> {
    let exec_start = render_service_exec_start(current_exe, config);
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

fn render_service_exec_start(current_exe: &Path, config: &TelegramModeConfig) -> String {
    let mut args = vec![
        current_exe.display().to_string(),
        "--workspace".to_string(),
        config.workspace.display().to_string(),
        "--provider".to_string(),
        config.route.provider_id.to_string(),
        "--model".to_string(),
        config.route.model_id.clone(),
        "--max-steps".to_string(),
        config.options.max_steps.to_string(),
        "--max-retries".to_string(),
        config.options.max_provider_retries.to_string(),
        "--timeout-secs".to_string(),
        config.options.provider_timeout_secs.to_string(),
        "telegram".to_string(),
    ];
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

fn render_service_env_file(config: &TelegramModeConfig) -> Result<String> {
    let workspace = config.workspace.display().to_string();
    for value in [
        config.token.as_str(),
        config.api_key.as_str(),
        workspace.as_str(),
        config.route.provider_id.as_str(),
        config.route.model_id.as_str(),
    ] {
        if value.contains('\n') {
            return Err(anyhow::anyhow!(
                "Service configuration contains a newline, which cannot be written safely."
            ));
        }
    }

    Ok(format!(
        "{header}
{managed_key}=1
{token_key}={token}
{api_key_key}={api_key}
{workspace_key}={workspace}
{provider_key}={provider}
{model_key}={model}
",
        header = TOPAGENT_MANAGED_HEADER,
        managed_key = TOPAGENT_SERVICE_MANAGED_KEY,
        token = quote_env_value(&config.token),
        api_key = quote_env_value(&config.api_key),
        workspace_key = TOPAGENT_WORKSPACE_KEY,
        workspace = quote_env_value(&workspace),
        provider_key = TOPAGENT_PROVIDER_KEY,
        provider = quote_env_value(config.route.provider_id.as_str()),
        model_key = TOPAGENT_MODEL_KEY,
        model = quote_env_value(&config.route.model_id),
        api_key_key = OPENROUTER_API_KEY_KEY,
        token_key = TELEGRAM_BOT_TOKEN_KEY,
    ))
}

fn quote_env_value(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '$' => quoted.push_str("\\$"),
            '`' => quoted.push_str("\\`"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
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

// ── systemd interface ──

fn run_systemctl_user(args: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `systemctl --user {}`", args.join(" ")))
}

fn ensure_systemd_user_available() -> Result<()> {
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

fn load_service_status_snapshot() -> Result<ServiceStatusSnapshot> {
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

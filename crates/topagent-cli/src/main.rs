// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod progress;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Duration;
use topagent_core::{
    channel::{ChannelAdapter, OutgoingMessage},
    context::ExecutionContext,
    create_provider,
    model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory},
    tools::default_tools,
    Agent, CancellationToken, ProgressCallback, ProgressUpdate, RuntimeOptions, TelegramAdapter,
    POLL_TIMEOUT_SECS,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::progress::LiveProgress;

const TELEGRAM_SERVICE_UNIT_NAME: &str = "topagent-telegram.service";
const TOPAGENT_MANAGED_HEADER: &str =
    "# Managed by TopAgent. Safe to remove with `topagent service uninstall`.";
const TOPAGENT_SERVICE_MANAGED_KEY: &str = "TOPAGENT_SERVICE_MANAGED";
const TOPAGENT_WORKSPACE_KEY: &str = "TOPAGENT_WORKSPACE";
const TOPAGENT_PROVIDER_KEY: &str = "TOPAGENT_PROVIDER";
const TOPAGENT_MODEL_KEY: &str = "TOPAGENT_MODEL";

#[derive(Parser)]
#[command(
    author,
    version,
    about = "topagent: minimal coding agent",
    arg_required_else_help = true
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "OpenRouter API key (or OPENROUTER_API_KEY)"
    )]
    api_key: Option<String>,

    #[arg(
        long,
        global = true,
        default_value = "openrouter",
        help = "Provider to use"
    )]
    provider: String,

    #[arg(
        long,
        global = true,
        help = "Model to use (overrides the default model)"
    )]
    model: Option<String>,

    #[arg(
        long = "workspace",
        alias = "cwd",
        global = true,
        help = "Workspace/repo directory override"
    )]
    workspace: Option<PathBuf>,

    #[arg(long, global = true, help = "Maximum steps for the agent loop")]
    max_steps: Option<usize>,

    #[arg(long, global = true, help = "Maximum provider retries")]
    max_retries: Option<usize>,

    #[arg(long, global = true, help = "Provider timeout in seconds")]
    timeout_secs: Option<u64>,

    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(help = "Instruction for one-shot mode")]
    instruction: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    Telegram {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    /// Remove the installed TopAgent binary and stop running processes.
    Uninstall,
    #[command(hide = true)]
    Run { instruction: String },
}

#[derive(Subcommand)]
enum ServiceCommands {
    Install {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
    Status,
    Uninstall,
}

#[derive(Debug, Clone)]
struct TelegramModeConfig {
    token: String,
    api_key: String,
    route: ModelRoute,
    workspace: PathBuf,
    options: RuntimeOptions,
}

#[derive(Debug, Clone)]
struct ServicePaths {
    unit_dir: PathBuf,
    unit_path: PathBuf,
    env_dir: PathBuf,
    env_path: PathBuf,
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

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    let Cli {
        api_key,
        provider,
        model,
        workspace,
        max_steps,
        max_retries,
        timeout_secs,
        command,
        instruction,
    } = cli;

    match command {
        Some(Commands::Uninstall) => return run_uninstall(),
        Some(Commands::Service { command }) => {
            return run_service_command(
                command,
                api_key,
                provider,
                model,
                workspace,
                max_steps,
                max_retries,
                timeout_secs,
            )
        }
        Some(Commands::Telegram { token }) => run_telegram(
            token,
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
        ),
        Some(Commands::Run { instruction }) => run_one_shot(
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
            instruction,
        ),
        None => {
            let instruction = instruction.ok_or_else(|| {
                anyhow::anyhow!("Instruction required. Run: topagent \"summarize this repository\"")
            })?;
            run_one_shot(
                api_key,
                provider,
                model,
                workspace,
                max_steps,
                max_retries,
                timeout_secs,
                instruction,
            )
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn run_uninstall() -> Result<()> {
    let my_pid = std::process::id();

    // Stop running topagent processes (excluding ourselves).
    let stopped = match std::process::Command::new("pgrep")
        .args(["-f", "topagent telegram"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .filter(|&pid| pid != my_pid)
                .collect();
            let mut killed = 0;
            for pid in &pids {
                let status = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
                if status.is_ok_and(|s| s.success()) {
                    killed += 1;
                }
            }
            killed
        }
        _ => 0,
    };

    if stopped > 0 {
        println!(
            "Stopped {} running TopAgent process{}.",
            stopped,
            if stopped == 1 { "" } else { "es" }
        );
    }

    // Remove the installed binary.
    let exe = std::env::current_exe().context("cannot determine binary path")?;
    let exe = exe.canonicalize().context("cannot resolve binary path")?;

    if exe.exists() {
        std::fs::remove_file(&exe)
            .with_context(|| format!("failed to remove {}", exe.display()))?;
        println!("Removed {}.", exe.display());
    } else {
        println!("Binary already absent: {}", exe.display());
    }

    println!();
    println!("TopAgent uninstalled.");
    println!();
    println!("Not removed (manual cleanup if needed):");
    println!("  - Source repo / workspace directories");
    println!("  - OPENROUTER_API_KEY / TELEGRAM_BOT_TOKEN in your shell profile");
    println!("  - Rotate any exposed API keys or bot tokens if needed");

    Ok(())
}

fn build_runtime_options(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> RuntimeOptions {
    RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(3))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120))
}

fn resolve_workspace_path(workspace: Option<PathBuf>) -> Result<PathBuf> {
    resolve_workspace_path_with_current_dir(workspace, std::env::current_dir())
}

fn resolve_workspace_path_with_current_dir(
    workspace: Option<PathBuf>,
    current_dir: std::io::Result<PathBuf>,
) -> Result<PathBuf> {
    let workspace = match workspace {
        Some(path) => path,
        None => current_dir.context(
            "Failed to determine the current directory. Run TopAgent from your repo or pass --workspace /path/to/repo.",
        )?,
    };

    if !workspace.exists() {
        return Err(anyhow::anyhow!(
            "Workspace path does not exist: {}. Run TopAgent from a repo directory or pass --workspace /path/to/repo.",
            workspace.display()
        ));
    }

    if !workspace.is_dir() {
        return Err(anyhow::anyhow!(
            "Workspace path is not a directory: {}",
            workspace.display()
        ));
    }

    workspace.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Workspace path is not accessible: {} ({})",
            workspace.display(),
            e
        )
    })
}

fn require_openrouter_api_key(api_key: Option<String>) -> Result<String> {
    let api_key = api_key
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if api_key.is_empty() {
        return Err(anyhow::anyhow!(
            "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY"
        ));
    }

    Ok(api_key)
}

fn require_telegram_token(token: Option<String>) -> Result<String> {
    let token = token
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN"
        ));
    }

    if !token.contains(':') {
        return Err(anyhow::anyhow!(
            "Telegram bot token looks invalid. Expected something like 123456:ABCdef..."
        ));
    }

    Ok(token)
}

fn install_ctrlc_handler(
    cancel_token: CancellationToken,
    progress_callback: ProgressCallback,
) -> Result<()> {
    let interrupt_count = Arc::new(AtomicUsize::new(0));
    ctrlc::set_handler(move || {
        let count = interrupt_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            cancel_token.cancel();
            progress_callback(ProgressUpdate::stopping());
        } else {
            eprintln!("status: forcing exit");
            std::process::exit(130);
        }
    })
    .context("Failed to install Ctrl-C handler")
}

fn run_one_shot(
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
    instruction: String,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    let cancel_token = CancellationToken::new();
    let ctx = ExecutionContext::new(workspace).with_cancel_token(cancel_token.clone());
    let options = build_runtime_options(max_steps, max_retries, timeout_secs);
    let route = build_route(provider, model)?;
    let api_key = require_openrouter_api_key(api_key)?;

    info!(
        "starting one-shot run | provider: {} | model: {} | workspace: {}",
        route.provider_id,
        route.model_id,
        ctx.workspace_root.display()
    );
    info!("instruction: {}", instruction);

    let provider = create_provider(
        &route,
        &api_key,
        default_tools().specs(),
        options.provider_timeout_secs,
    )?;

    let heartbeat_interval = Duration::from_secs(options.progress_heartbeat_secs);
    let mut agent = Agent::with_options(provider, default_tools().into_inner(), options);
    let progress = LiveProgress::for_cli(heartbeat_interval);
    let progress_callback = progress.callback();
    install_ctrlc_handler(cancel_token, progress_callback.clone())?;
    agent.set_progress_callback(Some(progress_callback));
    let result = agent.run(&ctx, &instruction);
    agent.set_progress_callback(None);
    progress.wait();

    match result {
        Ok(result) => {
            println!("{}", result);
            Ok(())
        }
        Err(topagent_core::Error::Stopped(_)) => {
            info!("one-shot run stopped by user");
            std::process::exit(130);
        }
        Err(e) => {
            error!("agent error: {}", e);
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn build_route(provider: String, model: Option<String>) -> Result<ModelRoute> {
    let provider_id = ProviderId::parse(&provider).map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut route = RoutingPolicy::select_route(TaskCategory::Default, model.as_deref());
    route.provider_id = provider_id;
    Ok(route)
}

fn run_service_command(
    command: ServiceCommands,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    match command {
        ServiceCommands::Install { token } => run_service_install(
            token,
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
        ),
        ServiceCommands::Status => run_service_status(),
        ServiceCommands::Uninstall => run_service_uninstall(),
    }
}

fn resolve_telegram_mode_config(
    token: Option<String>,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<TelegramModeConfig> {
    Ok(TelegramModeConfig {
        token: require_telegram_token(token)?,
        api_key: require_openrouter_api_key(api_key)?,
        route: build_route(provider, model)?,
        workspace: resolve_workspace_path(workspace)?,
        options: build_runtime_options(max_steps, max_retries, timeout_secs),
    })
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

fn service_paths() -> Result<ServicePaths> {
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
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;

    std::fs::create_dir_all(&paths.unit_dir)
        .with_context(|| format!("failed to create {}", paths.unit_dir.display()))?;
    std::fs::create_dir_all(&paths.env_dir)
        .with_context(|| format!("failed to create {}", paths.env_dir.display()))?;

    let current_exe = std::env::current_exe()
        .context("cannot determine binary path for service install")?
        .canonicalize()
        .context("cannot resolve binary path for service install")?;

    let env_contents = render_service_env_file(&config)?;
    let unit_contents = render_service_unit_file(&current_exe, &config, &paths)?;
    write_managed_file(&paths.env_path, &env_contents, true)?;
    write_managed_file(&paths.unit_path, &unit_contents, false)?;

    run_systemctl_user_checked(&["daemon-reload"], "reload the systemd user daemon")?;
    run_systemctl_user_checked(
        &["enable", "--now", TELEGRAM_SERVICE_UNIT_NAME],
        "enable and start the TopAgent Telegram service",
    )?;

    println!("TopAgent Telegram service installed and started.");
    println!("Unit: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Unit file: {}", paths.unit_path.display());
    println!("Env file: {}", paths.env_path.display());
    println!("Workspace: {}", config.workspace.display());
    println!("Inspect:");
    println!("  topagent service status");
    println!("  systemctl --user status {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("  journalctl --user -u {} -f", TELEGRAM_SERVICE_UNIT_NAME);

    Ok(())
}

fn run_service_status() -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let systemd_probe = ensure_systemd_user_available();
    let snapshot_result = if systemd_probe.is_ok() {
        Some(load_service_status_snapshot())
    } else {
        None
    };
    let snapshot = snapshot_result
        .as_ref()
        .and_then(|result| result.as_ref().ok());

    let installed = snapshot
        .as_ref()
        .and_then(|status| status.load_state.as_deref())
        .map(|state| state != "not-found")
        .unwrap_or(paths.unit_path.exists());
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

    println!("TopAgent Telegram service");
    println!("Installed: {}", yes_no(installed));
    if let (Some(enabled), Some(active)) = (enabled, active) {
        println!("Enabled: {}", yes_no(enabled));
        println!("Running: {}", yes_no(active));
    } else {
        println!("Enabled: unknown");
        println!("Running: unknown");
    }
    println!("Unit: {}", TELEGRAM_SERVICE_UNIT_NAME);
    println!("Unit file: {}", unit_path.display());
    println!("Env file: {}", paths.env_path.display());

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

    if let Some(status) = &snapshot {
        if let Some(active_state) = &status.active_state {
            let sub_state = status.sub_state.as_deref().unwrap_or("unknown");
            println!("Last state: {} ({})", active_state, sub_state);
        }
        if active != Some(true) {
            if let Some(result) = &status.result {
                if result != "success" {
                    println!("Last result: {}", result);
                }
            }
            if let Some(exit_status) = &status.exec_main_status {
                if exit_status != "0" {
                    println!("Exit status: {}", exit_status);
                }
            }
            if installed {
                println!(
                    "Inspect logs: journalctl --user -u {} -n 50 --no-pager",
                    TELEGRAM_SERVICE_UNIT_NAME
                );
            }
        }
    } else if let Some(Err(err)) = snapshot_result {
        println!("Service state: unable to inspect");
        println!("Hint: {}", err);
    } else if let Err(err) = systemd_probe {
        println!("Systemd user mode: unavailable");
        println!("Hint: {}", err);
    }

    Ok(())
}

fn run_service_uninstall() -> Result<()> {
    let paths = service_paths()?;
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let mut stopped = String::from("not attempted");
    let mut disabled = String::from("not attempted");

    if systemd_available.is_ok() {
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
    } else if let Err(err) = &systemd_available {
        let note = format!("not attempted ({})", err);
        stopped = note.clone();
        disabled = note;
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

    let mut daemon_reload = String::from("not needed");
    if systemd_available.is_ok() {
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

    println!("TopAgent Telegram service uninstall");
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
TELEGRAM_BOT_TOKEN={token}
OPENROUTER_API_KEY={api_key}
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

fn read_managed_env_metadata(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_string(), parse_env_value(raw_value.trim()));
    }
    Ok(values)
}

fn parse_env_value(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut unescaped = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    unescaped.push(next);
                }
            } else {
                unescaped.push(ch);
            }
        }
        return unescaped;
    }
    value.to_string()
}

fn assert_managed_or_absent(path: &Path, label: &str) -> Result<()> {
    if !path.exists() || is_topagent_managed_file(path)? {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "Refusing to overwrite existing {} at {} because it was not created by TopAgent. Move it aside or remove it, then run `topagent service install` again.",
        label,
        path.display()
    ))
}

fn is_topagent_managed_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(contents
        .lines()
        .any(|line| line.trim() == TOPAGENT_MANAGED_HEADER)
        || contents.contains(&format!("{TOPAGENT_SERVICE_MANAGED_KEY}=1")))
}

fn write_managed_file(path: &Path, contents: &str, private: bool) -> Result<()> {
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if private { 0o600 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    Ok(())
}

fn remove_managed_file(path: &Path, label: &str) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    if !is_topagent_managed_file(path)? {
        return Ok(None);
    }

    std::fs::remove_file(path)
        .with_context(|| format!("failed to remove {} {}", label, path.display()))?;
    Ok(Some(format!("{} {}", label, path.display())))
}

fn remove_managed_env_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let env_values = read_managed_env_metadata(path)?;
    if env_values
        .get(TOPAGENT_SERVICE_MANAGED_KEY)
        .map(String::as_str)
        != Some("1")
    {
        return Ok(None);
    }

    std::fs::remove_file(path)
        .with_context(|| format!("failed to remove env file {}", path.display()))?;
    Ok(Some(format!("env file {}", path.display())))
}

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
            "systemd user services are unavailable. `topagent service` currently supports Linux systemd user services only. {}",
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

fn run_telegram(
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
    let token = config.token;
    let workspace = config.workspace;
    let ctx = ExecutionContext::new(workspace);
    let workspace_label = ctx.workspace_root.display().to_string();
    let options = config.options;
    let api_key = config.api_key;
    let route = config.route;
    let adapter = TelegramAdapter::new(&token);

    match adapter.check_webhook() {
        Ok(true) => {
            return Err(anyhow::anyhow!(
                "Telegram webhook is configured. Please remove it before using long polling.\n\
                 Use deleteWebhook to disable the webhook: https://core.telegram.org/bots/api#deletewebhook"
            ));
        }
        Ok(false) => {}
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to check Telegram webhook state: {}. Check the bot token and network access.",
                e
            ));
        }
    }

    let bot_info = adapter.get_me().map_err(|e| {
        anyhow::anyhow!(
            "Failed to validate bot token (getMe failed): {}. \
             Make sure TELEGRAM_BOT_TOKEN is correct.",
            e
        )
    })?;

    info!(
        "starting Telegram mode | provider: {} | model: {} | workspace: {}",
        route.provider_id, route.model_id, workspace_label
    );
    info!(
        "bot: @{} (id: {}) | private text chats only | send /start in a private chat",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
    );

    let provider_label = route.provider_id.clone();
    let model_label = route.model_id.clone();
    let mut session_manager = ChatSessionManager::new(route, api_key, options);
    let mut offset = 0i64;
    let mut polling_retries = 0usize;

    info!("telegram polling started");

    loop {
        session_manager.collect_finished_tasks();
        match adapter.get_updates(Some(offset), Some(POLL_TIMEOUT_SECS), Some(&["message"])) {
            Ok(updates) => {
                if polling_retries > 0 {
                    info!(
                        "telegram polling recovered after {} retries",
                        polling_retries
                    );
                    session_manager.notify_polling_recovered();
                }
                polling_retries = 0;
                session_manager.collect_finished_tasks();
                for update in updates {
                    session_manager.collect_finished_tasks();
                    let Some(msg) = &update.message else { continue };
                    offset = update.update_id + 1;
                    let chat_id = msg.chat.id;
                    let message_id = msg.message_id;

                    if msg.chat.chat_type != "private" {
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: "This bot currently supports private chats only. Open a private chat with the bot and try again.".to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    }

                    let Some(text) = msg.text.clone() else {
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: "This bot currently supports text messages only.".to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    };

                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }

                    info!("received from chat {}: {}", chat_id, text);

                    if text == "/start" || text == "/help" {
                        let reply = if text == "/start" {
                            format!(
                                "TopAgent\n\n\
                                 Workspace: {}\n\
                                 Provider: {} | Model: {}\n\
                                 Mode: private text chats only\n\n\
                                 Commands:\n\
                                 /help - show this message\n\
                                 /stop - stop the current task\n\
                                 /reset - clear conversation history\n\n\
                                 Try this first message:\n\
                                 Summarize this repository and tell me the main entry points.",
                                workspace_label, provider_label, model_label
                            )
                        } else {
                            format!(
                                "TopAgent\n\n\
                                 Workspace: {}\n\
                                 Send a plain text task about this workspace.\n\
                                 /stop requests cancellation of the current task.\n\
                                 /reset clears your conversation history.",
                                workspace_label
                            )
                        };
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: reply,
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    }

                    if text == "/stop" {
                        let reply = if session_manager.stop_chat(chat_id) {
                            "Stopping current task...".to_string()
                        } else {
                            "No task is currently running.".to_string()
                        };
                        send_telegram_chunks(&adapter, chat_id, vec![reply]);
                        continue;
                    }

                    if text == "/reset" {
                        let reply = if session_manager.is_task_running(chat_id) {
                            "A task is still running. Send /stop and wait for it to finish before /reset."
                                .to_string()
                        } else {
                            session_manager.reset_chat(chat_id);
                            "Conversation history cleared.".to_string()
                        };
                        send_telegram_chunks(&adapter, chat_id, vec![reply]);
                        continue;
                    }

                    let response = session_manager.start_message(&ctx, &adapter, chat_id, text);
                    send_telegram_chunks(&adapter, chat_id, response);
                    let _ = adapter.acknowledge(chat_id, message_id);
                }
            }
            Err(e) => {
                polling_retries += 1;
                session_manager.notify_polling_retry();
                let backoff = std::cmp::min(5 * polling_retries as u64, 30);
                if polling_retries <= 3 {
                    warn!(
                        "telegram polling failed: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                } else {
                    error!(
                        "telegram polling sustained failure: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(backoff));
            }
        }
    }
}

fn send_telegram_chunks(adapter: &TelegramAdapter, chat_id: i64, chunks: Vec<String>) {
    for chunk in chunks {
        let outgoing = OutgoingMessage {
            chat_id,
            text: chunk,
        };
        if let Err(e) = adapter.send_message(outgoing) {
            error!("failed to send message: {}", e);
        }
    }
}

struct ChatSessionManager {
    route: ModelRoute,
    api_key: String,
    options: RuntimeOptions,
    sessions: HashMap<i64, SessionState>,
    completed_tx: mpsc::Sender<CompletedChatTask>,
    completed_rx: mpsc::Receiver<CompletedChatTask>,
}

enum SessionState {
    Idle(Agent),
    Running(RunningChatTask),
}

struct RunningChatTask {
    cancel_token: CancellationToken,
    progress_callback: Option<ProgressCallback>,
}

struct CompletedChatTask {
    chat_id: i64,
    agent: Agent,
}

impl ChatSessionManager {
    fn new(route: ModelRoute, api_key: String, options: RuntimeOptions) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        Self {
            route,
            api_key,
            options,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
        }
    }

    fn create_agent(&self) -> Agent {
        let provider = create_provider(
            &self.route,
            &self.api_key,
            default_tools().specs(),
            self.options.provider_timeout_secs,
        )
        .expect("failed to create provider");
        let tools = default_tools();
        Agent::with_options(provider, tools.into_inner(), self.options.clone())
    }

    fn collect_finished_tasks(&mut self) {
        while let Ok(task) = self.completed_rx.try_recv() {
            self.sessions
                .insert(task.chat_id, SessionState::Idle(task.agent));
        }
    }

    fn is_task_running(&self, chat_id: i64) -> bool {
        matches!(self.sessions.get(&chat_id), Some(SessionState::Running(_)))
    }

    fn stop_chat(&mut self, chat_id: i64) -> bool {
        let Some(SessionState::Running(task)) = self.sessions.get(&chat_id) else {
            return false;
        };

        task.cancel_token.cancel();
        if let Some(callback) = &task.progress_callback {
            callback(ProgressUpdate::stopping());
        }
        true
    }

    fn notify_polling_retry(&self) {
        self.broadcast_progress(ProgressUpdate::retrying(
            "Telegram polling failed, retrying connection...",
        ));
    }

    fn notify_polling_recovered(&self) {
        self.broadcast_progress(ProgressUpdate::working(
            "Telegram connection restored. Task still running...",
        ));
    }

    fn broadcast_progress(&self, update: ProgressUpdate) {
        for session in self.sessions.values() {
            let SessionState::Running(task) = session else {
                continue;
            };

            if let Some(callback) = &task.progress_callback {
                callback(update.clone());
            }
        }
    }

    fn reset_chat(&mut self, chat_id: i64) {
        self.sessions.remove(&chat_id);
    }

    fn start_message(
        &mut self,
        ctx: &ExecutionContext,
        adapter: &TelegramAdapter,
        chat_id: i64,
        text: &str,
    ) -> Vec<String> {
        self.collect_finished_tasks();
        if self.is_task_running(chat_id) {
            return vec![
                "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                    .to_string(),
            ];
        }

        let heartbeat_interval = Duration::from_secs(self.options.progress_heartbeat_secs);
        let mut agent = match self.sessions.remove(&chat_id) {
            Some(SessionState::Idle(agent)) => agent,
            Some(SessionState::Running(task)) => {
                self.sessions.insert(chat_id, SessionState::Running(task));
                return vec![
                    "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                        .to_string(),
                ];
            }
            None => self.create_agent(),
        };

        let cancel_token = CancellationToken::new();
        let run_ctx = ctx.clone().with_cancel_token(cancel_token.clone());
        let progress =
            match LiveProgress::for_telegram(heartbeat_interval, adapter.clone(), chat_id) {
                Ok(progress) => Some(progress),
                Err(err) => {
                    error!("failed to start Telegram live progress: {}", err);
                    None
                }
            };
        let progress_callback = progress.as_ref().map(|progress| progress.callback());
        let worker_progress_callback = progress_callback.clone();
        let completed_tx = self.completed_tx.clone();
        let adapter = adapter.clone();
        let instruction = text.to_string();

        thread::spawn(move || {
            let has_progress = worker_progress_callback.is_some();
            if let Some(callback) = &worker_progress_callback {
                agent.set_progress_callback(Some(callback.clone()));
            }

            let result = agent.run(&run_ctx, &instruction);
            agent.set_progress_callback(None);

            if let Some(progress) = progress {
                progress.wait();
            }

            match result {
                Ok(response) => {
                    let max_len = 4000;
                    let chunks = if response.len() <= max_len {
                        vec![response]
                    } else {
                        topagent_core::channel::telegram::chunk_text(&response, max_len)
                    };
                    send_telegram_chunks(&adapter, chat_id, chunks);
                }
                Err(topagent_core::Error::Stopped(_)) => {}
                Err(e) => {
                    // When progress is active, the status message already shows the
                    // failure via ProgressUpdate::failed. Don't send a duplicate error.
                    if !has_progress {
                        send_telegram_chunks(&adapter, chat_id, vec![format!("Error: {}", e)]);
                    }
                }
            }

            let _ = completed_tx.send(CompletedChatTask { chat_id, agent });
        });

        self.sessions.insert(
            chat_id,
            SessionState::Running(RunningChatTask {
                cancel_token,
                progress_callback,
            }),
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_workspace_path_with_current_dir, ChatSessionManager, RunningChatTask, SessionState,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::{
        CancellationToken, ModelRoute, ProgressKind, ProgressUpdate, RuntimeOptions,
    };

    #[test]
    fn test_workspace_defaults_to_current_directory_for_one_shot_and_telegram() {
        let temp = TempDir::new().unwrap();
        let resolved =
            resolve_workspace_path_with_current_dir(None, Ok(temp.path().to_path_buf())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_override_beats_current_directory_for_one_shot_and_telegram() {
        let current = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(override_dir.path().to_path_buf()),
            Ok(current.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_resolution_fails_when_current_directory_is_unavailable() {
        let err = resolve_workspace_path_with_current_dir(
            None,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Failed to determine the current directory"));
    }

    #[test]
    fn test_workspace_override_ignores_invalid_current_directory() {
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(PathBuf::from(override_dir.path())),
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_stop_chat_cancels_running_task_and_emits_stopping_progress() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let cancel_token = CancellationToken::new();
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: Some(progress_callback),
            }),
        );

        assert!(manager.stop_chat(42));
        assert!(cancel_token.is_cancelled());
        assert!(updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update == &ProgressUpdate::stopping()));
    }

    #[test]
    fn test_stop_chat_returns_false_when_idle() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        assert!(!manager.stop_chat(42));
    }

    #[test]
    fn test_notify_polling_retry_emits_retrying_progress_to_running_chat() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
        );

        manager.notify_polling_retry();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Retrying
                && update
                    .message
                    .contains("Telegram polling failed, retrying connection")
        }));
    }

    #[test]
    fn test_notify_polling_recovered_emits_working_progress_to_running_chat() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
        );

        manager.notify_polling_recovered();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Working
                && update
                    .message
                    .contains("Telegram connection restored. Task still running")
        }));
    }
}

// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod config;
mod managed_files;
mod progress;
mod service;
mod telegram;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, create_provider, tools::default_tools, Agent, CancellationToken,
    ProgressCallback, ProgressUpdate,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::config::{
    build_route, build_runtime_options, require_openrouter_api_key, resolve_workspace_path,
    CliParams,
};
use crate::progress::LiveProgress;
use crate::service::{run_install, run_service_command, run_status, run_uninstall};
use crate::telegram::run_telegram;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "TopAgent local coding agent",
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
        help = "Workspace directory override"
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

    #[arg(help = "Run a one-shot task")]
    instruction: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up and start the TopAgent Telegram background service.
    Install,
    /// Show TopAgent setup and service status.
    Status,
    /// Run the Telegram bot in the foreground.
    Telegram {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
    /// Manage the installed Telegram background service.
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    /// Remove the installed TopAgent setup and, when applicable, the installed binary.
    Uninstall,
    #[command(hide = true)]
    Run { instruction: String },
}

#[derive(Subcommand)]
pub(crate) enum ServiceCommands {
    /// Install and start the Telegram background service.
    Install {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
    /// Show Telegram service status.
    Status,
    /// Start the installed Telegram background service.
    Start,
    /// Stop the installed Telegram background service.
    Stop,
    /// Restart the installed Telegram background service.
    Restart,
    /// Remove the Telegram background service and managed env file.
    Uninstall,
}

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    let command = cli.command;
    let instruction = cli.instruction;
    let params = CliParams {
        api_key: cli.api_key,
        provider: cli.provider,
        model: cli.model,
        workspace: cli.workspace,
        max_steps: cli.max_steps,
        max_retries: cli.max_retries,
        timeout_secs: cli.timeout_secs,
    };

    match command {
        Some(Commands::Install) => run_install(params),
        Some(Commands::Status) => run_status(),
        Some(Commands::Uninstall) => run_uninstall(),
        Some(Commands::Service { command }) => run_service_command(command, params),
        Some(Commands::Telegram { token }) => run_telegram(token, params),
        Some(Commands::Run { instruction }) => run_one_shot(params, instruction),
        None => {
            let instruction = instruction.ok_or_else(|| {
                anyhow::anyhow!("Instruction required. Run: topagent \"summarize this repository\"")
            })?;
            run_one_shot(params, instruction)
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
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

fn run_one_shot(params: CliParams, instruction: String) -> Result<()> {
    let workspace = resolve_workspace_path(params.workspace)?;
    let cancel_token = CancellationToken::new();
    let ctx = ExecutionContext::new(workspace).with_cancel_token(cancel_token.clone());
    let options = build_runtime_options(params.max_steps, params.max_retries, params.timeout_secs);
    let route = build_route(params.provider, params.model)?;
    let api_key = require_openrouter_api_key(params.api_key)?;

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

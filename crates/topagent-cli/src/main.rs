// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod config;
mod managed_files;
mod memory;
mod progress;
mod service;
mod telegram;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, create_provider, tools::default_tools, Agent, ApprovalMailbox,
    ApprovalMailboxMode, ApprovalRequest, CancellationToken, ProgressCallback, ProgressUpdate,
};
use tracing::warn;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::config::{
    build_route, build_runtime_options, require_openrouter_api_key, resolve_workspace_path,
    CliParams,
};
use crate::memory::WorkspaceMemory;
use crate::progress::LiveProgress;
use crate::service::{run_install, run_service_command, run_status, run_uninstall};
use crate::telegram::run_telegram;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ToolAuthoringMode {
    On,
    Off,
}

impl ToolAuthoringMode {
    fn is_enabled(self) -> bool {
        matches!(self, Self::On)
    }
}

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

    #[arg(
        long = "tool-authoring",
        global = true,
        value_enum,
        help = "Enable or disable generated-tool authoring tools for this run or installed service"
    )]
    tool_authoring: Option<ToolAuthoringMode>,

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
        generated_tool_authoring: cli.tool_authoring.map(ToolAuthoringMode::is_enabled),
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
    let interactive_approvals = io::stdin().is_terminal() && io::stderr().is_terminal();
    let approval_mailbox = build_cli_approval_mailbox(interactive_approvals);
    let mut ctx = ExecutionContext::new(workspace)
        .with_cancel_token(cancel_token.clone())
        .with_approval_mailbox(approval_mailbox);
    let options = build_runtime_options(params.max_steps, params.max_retries, params.timeout_secs)
        .with_generated_tool_authoring(params.generated_tool_authoring.unwrap_or(false));
    let route = build_route(params.provider, params.model)?;
    let api_key = require_openrouter_api_key(params.api_key)?;
    let workspace_memory = WorkspaceMemory::new(ctx.workspace_root.clone());

    if let Err(err) = workspace_memory.consolidate_memory_if_needed() {
        warn!("failed to consolidate workspace memory index: {}", err);
    }
    match workspace_memory.build_prompt(&instruction, None) {
        Ok(memory_prompt) => {
            if let Some(memory_context) = memory_prompt.prompt {
                ctx = ctx.with_memory_context(memory_context);
            }
        }
        Err(err) => {
            warn!("failed to load workspace memory context: {}", err);
        }
    }

    info!(
        "starting one-shot run | provider: {} | model: {} | workspace: {}",
        route.provider_id,
        route.model_id,
        ctx.workspace_root.display()
    );
    info!("instruction: {}", instruction);

    let tools = default_tools();
    let provider = create_provider(
        &route,
        &api_key,
        tools.specs(),
        options.provider_timeout_secs,
    )?;

    let heartbeat_interval = Duration::from_secs(options.progress_heartbeat_secs);
    let mut agent = Agent::with_route(provider, route, tools.into_inner(), options);
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
        Err(topagent_core::Error::ApprovalRequired(request)) => {
            error!("approval required during one-shot run: {}", request);
            eprintln!(
                "{}",
                format_cli_approval_required(&request, interactive_approvals)
            );
            std::process::exit(2);
        }
        Err(e) => {
            error!("agent error: {}", e);
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn build_cli_approval_mailbox(interactive: bool) -> ApprovalMailbox {
    let mode = if interactive {
        ApprovalMailboxMode::Wait
    } else {
        ApprovalMailboxMode::Immediate
    };
    let mailbox = ApprovalMailbox::new(mode);
    if interactive {
        let mailbox_for_prompt = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            let stdin = io::stdin();
            let mut stderr = io::stderr();
            let decision =
                prompt_for_cli_approval_with_io(&request, &mut stdin.lock(), &mut stderr)
                    .unwrap_or(false);
            let result = if decision {
                mailbox_for_prompt.approve(&request.id, Some("approved in one-shot CLI".into()))
            } else {
                mailbox_for_prompt.deny(&request.id, Some("denied in one-shot CLI".into()))
            };
            if let Err(err) = result {
                let _ = writeln!(
                    stderr,
                    "failed to resolve approval request {}: {}",
                    request.id, err
                );
            }
        }));
    }
    mailbox
}

fn prompt_for_cli_approval_with_io(
    request: &ApprovalRequest,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<bool> {
    writeln!(writer, "\n{}\n", request.render_details())?;
    loop {
        write!(writer, "Approve this action? [y/N]: ")?;
        writer.flush()?;

        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            writeln!(writer)?;
            return Ok(false);
        }

        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => {
                writeln!(writer, "Please answer yes or no.")?;
            }
        }
    }
}

fn format_cli_approval_required(request: &ApprovalRequest, interactive: bool) -> String {
    let mut message = request.render_details();
    if interactive {
        message.push_str(
            "\n\nThe operator declined or did not resolve the approval in this one-shot run.",
        );
    } else {
        message.push_str(
            "\n\nThis one-shot run is non-interactive, so the action was not executed. Re-run from an interactive terminal to approve it.",
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use topagent_core::ApprovalTriggerKind;

    fn sample_request() -> ApprovalRequest {
        ApprovalRequest {
            id: "apr-7".to_string(),
            action_kind: ApprovalTriggerKind::GitCommit,
            short_summary: "git commit: ship it".to_string(),
            exact_action: "git_commit(message=\"ship it\")".to_string(),
            reason: "commits publish a durable repo milestone".to_string(),
            scope_of_impact: "Creates a new git commit in the workspace repository.".to_string(),
            expected_effect: "Staged changes become a durable repo milestone.".to_string(),
            rollback_hint: Some("Use git revert or git reset if the commit was mistaken.".into()),
            created_at: std::time::SystemTime::now(),
        }
    }

    #[test]
    fn test_prompt_for_cli_approval_accepts_yes() {
        let request = sample_request();
        let mut reader = Cursor::new(b"yes\n".to_vec());
        let mut output = Vec::new();

        let approved = prompt_for_cli_approval_with_io(&request, &mut reader, &mut output).unwrap();

        assert!(approved);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("Approve this action?"));
    }

    #[test]
    fn test_prompt_for_cli_approval_defaults_to_no_on_blank() {
        let request = sample_request();
        let mut reader = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();

        let approved = prompt_for_cli_approval_with_io(&request, &mut reader, &mut output).unwrap();

        assert!(!approved);
    }

    #[test]
    fn test_format_cli_approval_required_mentions_non_interactive_mode() {
        let request = sample_request();
        let message = format_cli_approval_required(&request, false);

        assert!(message.contains("non-interactive"));
        assert!(message.contains("apr-7"));
    }
}

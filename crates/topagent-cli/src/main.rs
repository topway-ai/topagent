// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod checkpoint;
mod config;
mod doctor;
mod learning;
mod managed_files;
mod memory;
mod openrouter_models;
mod operational_paths;
mod progress;
mod run_setup;
mod service;
mod telegram;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use topagent_core::{
    ApprovalMailbox, ApprovalMailboxMode, ApprovalRequest, CancellationToken, ProgressCallback,
    ProgressUpdate, ProviderKind, WorkspaceCheckpointStore, context::ExecutionContext,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::checkpoint::run_checkpoint_command;
use crate::config::{
    CliParams, build_route_from_resolved, build_runtime_options, load_persisted_telegram_defaults,
    require_openrouter_api_key, resolve_runtime_model_selection, resolve_workspace_path,
};
use crate::doctor::run_doctor;
use crate::learning::{
    run_memory_command, run_observation_command, run_procedure_command, run_trajectory_command,
};
use crate::memory::{PromotionContext, promote_verified_task};
use crate::progress::LiveProgress;
use crate::run_setup::{build_agent, prepare_run_context, prepare_workspace_memory};
use crate::service::{
    run_install, run_model_command, run_service_command, run_status, run_uninstall,
};
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
        help = "OpenRouter API key (or OPENROUTER_API_KEY); use --opencode-api-key for Opencode"
    )]
    api_key: Option<String>,

    #[arg(long, global = true, help = "Opencode API key (or OPENCODE_API_KEY)")]
    opencode_api_key: Option<String>,

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
    #[command(visible_alias = "setup")]
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
    /// Inspect and change the configured model.
    Model {
        #[command(subcommand)]
        command: ModelCommands,
    },
    /// Inspect workspace memory layers.
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// Inspect and govern reusable procedures.
    Procedure {
        #[command(subcommand)]
        command: ProcedureCommands,
    },
    /// Inspect, review, and export saved trajectories.
    Trajectory {
        #[command(subcommand)]
        command: TrajectoryCommands,
    },
    /// Inspect observation records emitted during promotion.
    Observation {
        #[command(subcommand)]
        command: ObservationCommands,
    },
    /// Inspect and restore the latest workspace checkpoint.
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCommands,
    },
    /// Run health diagnostics on TopAgent setup, config, workspace, and tools.
    Doctor,
    /// Remove the installed TopAgent setup and, when applicable, the installed binary.
    Uninstall {
        /// Also remove workspace .topagent/ data, cache files, and auto-created workspace
        #[arg(long, short = 'p')]
        purge: bool,
    },
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
    Uninstall {
        /// Also remove workspace .topagent/ data and cache files
        #[arg(long, short = 'p')]
        purge: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ModelCommands {
    /// Show the configured model.
    Status,
    /// Set the configured model and restart the service when installed.
    Set { model_id: String },
    /// Pick the configured model interactively.
    Pick,
    /// Show the cached model list.
    List,
    /// Refresh the cached model list.
    Refresh,
}

#[derive(Subcommand)]
pub(crate) enum CheckpointCommands {
    /// Show the latest workspace checkpoint.
    Status,
    /// Show the diff between the latest checkpoint and the current workspace.
    Diff,
    /// Restore the latest checkpoint and clear persisted Telegram transcripts.
    Restore,
}

#[derive(Subcommand)]
pub(crate) enum MemoryCommands {
    /// Show workspace learning artifact status.
    Status,
    /// Lint USER.md and MEMORY.md for size, format, and content policy issues.
    Lint,
    /// Dry-run memory retrieval for an instruction and show recall provenance.
    Recall {
        #[arg(help = "Instruction to test recall for")]
        instruction: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProcedureCommands {
    /// List saved procedures.
    List {
        #[arg(long, help = "Include superseded and disabled procedures")]
        all: bool,
    },
    /// Show one saved procedure.
    Show { id: String },
    /// Remove superseded and disabled procedures.
    Prune,
    /// Mark a procedure disabled.
    Disable {
        id: String,
        #[arg(long, help = "Reason for disabling the procedure")]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum TrajectoryCommands {
    /// List saved trajectories.
    List,
    /// Show one saved trajectory.
    Show { id: String },
    /// Mark a trajectory ready for export after review.
    Review { id: String },
    /// Export a reviewed trajectory.
    Export { id: String },
}

#[derive(Subcommand)]
pub(crate) enum ObservationCommands {
    /// List recent observation records.
    List {
        #[arg(
            long,
            default_value = "20",
            help = "Maximum number of observations to show"
        )]
        limit: usize,
    },
    /// Show one observation record in detail.
    Show { id: String },
}

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    let command = cli.command;
    let instruction = cli.instruction;
    let params = CliParams {
        api_key: cli.api_key,
        opencode_api_key: cli.opencode_api_key,
        model: cli.model,
        workspace: cli.workspace,
        max_steps: cli.max_steps,
        max_retries: cli.max_retries,
        timeout_secs: cli.timeout_secs,
        generated_tool_authoring: cli.tool_authoring.map(ToolAuthoringMode::is_enabled),
    };

    match command {
        Some(Commands::Install) => run_install(params),
        Some(Commands::Status) => run_status(params),
        Some(Commands::Doctor) => run_doctor(params),
        Some(Commands::Model { command }) => run_model_command(command, params),
        Some(Commands::Memory { command }) => run_memory_command(command, params.workspace),
        Some(Commands::Procedure { command }) => run_procedure_command(command, params.workspace),
        Some(Commands::Trajectory { command }) => run_trajectory_command(command, params.workspace),
        Some(Commands::Observation { command }) => {
            run_observation_command(command, params.workspace)
        }
        Some(Commands::Checkpoint { command }) => run_checkpoint_command(command, params.workspace),
        Some(Commands::Uninstall { purge }) => run_uninstall(purge),
        Some(Commands::Service { command }) => run_service_command(command, params, false),
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
    let mut ctx = ExecutionContext::new(workspace.clone())
        .with_cancel_token(cancel_token.clone())
        .with_approval_mailbox(approval_mailbox)
        .with_workspace_checkpoint_store(WorkspaceCheckpointStore::new(workspace));
    let options = build_runtime_options(params.max_steps, params.max_retries, params.timeout_secs)
        .with_generated_tool_authoring(params.generated_tool_authoring.unwrap_or(false));
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let model_selection =
        resolve_runtime_model_selection(params.model, persisted_defaults.model.clone());
    let route = build_route_from_resolved(&model_selection.effective);
    let api_key = match route.provider {
        ProviderKind::OpenRouter => require_openrouter_api_key(params.api_key)?,
        ProviderKind::Opencode => {
            let defaults = load_persisted_telegram_defaults().unwrap_or_default();
            crate::config::require_opencode_api_key(
                params.opencode_api_key.or(defaults.opencode_api_key),
            )?
        }
    };
    let workspace_memory = prepare_workspace_memory(ctx.workspace_root.clone());
    let prepared_run = prepare_run_context(&ctx, &workspace_memory, &instruction, None);
    let loaded_procedure_files = prepared_run.loaded_procedure_files.clone();
    ctx = prepared_run.run_ctx;

    info!(
        "starting one-shot run | model: {} | workspace: {}",
        route.model_id,
        ctx.workspace_root.display()
    );
    info!("instruction: {}", instruction);

    let heartbeat_interval = Duration::from_secs(options.progress_heartbeat_secs);
    let distill_options = options.clone();
    let mut agent = build_agent(&route, &api_key, options);
    let progress = LiveProgress::for_cli(heartbeat_interval);
    let progress_callback = progress.callback();
    install_ctrlc_handler(cancel_token, progress_callback.clone())?;
    agent.set_progress_callback(Some(progress_callback));
    let result = agent.run(&ctx, &instruction);
    agent.set_progress_callback(None);
    progress.wait();

    match result {
        Ok(result) => {
            let mut final_output = result;
            if let Some(task_result) = agent.last_task_result().cloned() {
                match agent.plan().lock() {
                    Ok(plan) => match promote_verified_task(&PromotionContext {
                        memory: &workspace_memory,
                        ctx: &ctx,
                        options: &distill_options,
                        instruction: &instruction,
                        task_mode: agent.task_mode(),
                        task_result: &task_result,
                        plan: &plan.clone(),
                        durable_memory_written: agent.durable_memory_written_this_run(),
                        loaded_procedure_files: &loaded_procedure_files,
                    }) {
                        Ok(report) => {
                            if report.lesson_file.is_some()
                                || report.procedure_file.is_some()
                                || report.trajectory_file.is_some()
                            {
                                info!(
                                    lesson = report.lesson_file.as_deref().unwrap_or(""),
                                    procedure = report.procedure_file.as_deref().unwrap_or(""),
                                    trajectory = report.trajectory_file.as_deref().unwrap_or(""),
                                    "saved promoted workspace learning artifacts"
                                );
                            }
                            if !report.notes.is_empty() {
                                final_output.push_str("\n\n### Trust Notes\n");
                                for note in report.notes {
                                    final_output.push_str(&format!("- {}\n", note));
                                }
                            }
                        }
                        Err(err) => warn!("failed to promote verified task memory: {}", err),
                    },
                    Err(err) => warn!("failed to lock agent plan for distillation: {}", err),
                }
            }
            println!("{}", final_output);
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
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Approve this action?")
        );
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

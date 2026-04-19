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
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, ApprovalMailbox, ApprovalMailboxMode, ApprovalRequest,
    CancellationToken, ProgressCallback, ProgressUpdate, WorkspaceCheckpointStore,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::checkpoint::run_checkpoint_command;
use crate::config::{
    load_persisted_telegram_defaults, resolve_contract_summary, resolve_one_shot_config, CliParams,
    ResolvedContractSummary,
};
use crate::doctor::run_doctor;
use crate::learning::{
    run_memory_command, run_observation_command, run_procedure_command, run_trajectory_command,
};
use crate::memory::{promote_verified_task, PromotionContext};
use crate::progress::LiveProgress;
use crate::run_setup::{build_agent, prepare_run_context, prepare_workspace_memory};
use crate::service::{
    run_install, run_model_command, run_service_command, run_status, run_uninstall, run_upgrade,
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
    /// Inspect the resolved runtime contract (provider, model, keys, workspace, options).
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Run health diagnostics on TopAgent setup, config, workspace, and tools.
    Doctor,
    /// Inspect execution-session state: checkpoint, transcripts, and recovery readiness.
    Run {
        #[command(subcommand)]
        command: RunCommands,
    },
    /// Upgrade the TopAgent binary to the latest GitHub release and restart the service.
    Upgrade {
        /// Build from source via `cargo install --git` instead of downloading a release binary.
        #[arg(long)]
        use_cargo: bool,
    },
    /// Remove the installed TopAgent setup and, when applicable, the installed binary.
    Uninstall {
        /// Also remove workspace .topagent/ data, cache files, and auto-created workspace
        #[arg(long, short = 'p')]
        purge: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum RunCommands {
    /// Show execution-session state: checkpoint material, Telegram transcripts,
    /// service active/inactive, and recovery readiness. Never prints secrets.
    Status,
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
pub(crate) enum ConfigCommands {
    /// Show the fully resolved runtime contract: provider, model, API key presence,
    /// workspace, Telegram admission state, and runtime options. Never prints
    /// secret values — keys and tokens are shown as present/missing only.
    Inspect,
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
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Inspect => run_config_inspect(params),
        },
        Some(Commands::Model { command }) => run_model_command(command, params),
        Some(Commands::Memory { command }) => run_memory_command(command, params.workspace),
        Some(Commands::Procedure { command }) => run_procedure_command(command, params.workspace),
        Some(Commands::Trajectory { command }) => run_trajectory_command(command, params.workspace),
        Some(Commands::Observation { command }) => {
            run_observation_command(command, params.workspace)
        }
        Some(Commands::Checkpoint { command }) => run_checkpoint_command(command, params.workspace),
        Some(Commands::Run { command }) => match command {
            RunCommands::Status => run_session_status(params.workspace),
        },
        Some(Commands::Upgrade { use_cargo }) => run_upgrade(use_cargo),
        Some(Commands::Uninstall { purge }) => run_uninstall(purge),
        Some(Commands::Service { command }) => {
            let purge = matches!(command, crate::ServiceCommands::Uninstall { purge: true });
            run_service_command(command, params, purge)
        }
        Some(Commands::Telegram { token }) => run_telegram(token, params),
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

fn run_config_inspect(params: CliParams) -> Result<()> {
    let defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let summary = resolve_contract_summary(&params, &defaults);
    print!("{}", render_contract_summary(&summary));
    Ok(())
}

fn run_session_status(workspace_override: Option<PathBuf>) -> Result<()> {
    let workspace = crate::config::resolve_workspace_path(workspace_override)?;
    print!("{}", render_session_status(&workspace));
    Ok(())
}

/// Render a compact, secret-safe summary of execution-session state for a
/// given workspace directory. Shows checkpoint material, Telegram transcript
/// presence, and service state — never prints secret values.
pub(crate) fn render_session_status(workspace: &std::path::Path) -> String {
    let mut out = String::from("TopAgent run status\n\n");
    out.push_str(&format!("Workspace: {}\n", workspace.display()));

    // ── Service state ──
    let service_state = crate::service::query_service_active_state();
    out.push_str(&format!("\nService state:        {}\n", service_state));

    // ── Checkpoint ──
    let store = WorkspaceCheckpointStore::new(workspace.to_path_buf());
    match store.latest_status() {
        Ok(Some(status)) => {
            out.push_str(&format!(
                "\nCheckpoint:           present ({})\n",
                status.id
            ));
            let timestamp = format_session_time(status.created_at_unix_millis);
            out.push_str(&format!("  Created:            {}\n", timestamp));
            out.push_str(&format!(
                "  Captured paths:     {}\n",
                status.captured_paths.len()
            ));
        }
        Ok(None) => {
            out.push_str("\nCheckpoint:           none\n");
        }
        Err(err) => {
            out.push_str(&format!("\nCheckpoint:           error — {}\n", err));
        }
    }

    // ── Telegram transcripts ──
    let history_dir = workspace.join(".topagent").join("telegram-history");
    let transcript_count = if history_dir.is_dir() {
        std::fs::read_dir(&history_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .map(|ext| ext == "json")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };
    out.push_str(&format!(
        "\nTelegram transcripts: {} chat file{}\n",
        transcript_count,
        if transcript_count == 1 { "" } else { "s" }
    ));

    // ── Recovery guidance ──
    let has_checkpoint = matches!(store.latest_status(), Ok(Some(_)));
    let has_transcripts = transcript_count > 0;
    if has_checkpoint || has_transcripts {
        out.push_str("\nRecovery:\n");
        if has_checkpoint {
            out.push_str("  A checkpoint exists. Preview changes with: topagent checkpoint diff\n");
            out.push_str(
                "  Restore workspace and clear transcripts:   topagent checkpoint restore\n",
            );
        }
        if has_transcripts {
            out.push_str("  Clear per-chat transcripts via Telegram:   /reset (in each chat)\n");
        }
    }

    out.push_str(
        "\nNote: In-flight session state is not persisted. For run logs:\n  \
         journalctl --user -u topagent-telegram.service -n 50\n",
    );

    out
}

fn format_session_time(unix_millis: u128) -> String {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let timestamp = i64::try_from(unix_millis / 1000).unwrap_or(i64::MAX);
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| unix_millis.to_string())
}

pub(crate) fn render_contract_summary(summary: &ResolvedContractSummary) -> String {
    let mut out = String::from("TopAgent runtime contract\n\n");

    out.push_str(&format!("Provider:           {}\n", summary.provider));
    out.push_str(&format!(
        "Model:              {}  [{}]\n",
        summary.effective_model, summary.effective_model_source_label
    ));
    if let Some(ref default_model) = summary.configured_default_model {
        out.push_str(&format!(
            "Default model:      {}  [configured default]\n",
            default_model
        ));
    }
    match &summary.workspace {
        Ok(path) => out.push_str(&format!("Workspace:          {}\n", path.display())),
        Err(err) => out.push_str(&format!("Workspace:          error — {}\n", err)),
    }

    out.push_str("\nAPI keys:\n");
    out.push_str(&format!(
        "  OpenRouter:       {}\n",
        if summary.openrouter_key_present {
            "present"
        } else {
            "missing"
        }
    ));
    out.push_str(&format!(
        "  Opencode:         {}\n",
        if summary.opencode_key_present {
            "present"
        } else {
            "missing"
        }
    ));

    out.push_str("\nTelegram:\n");
    out.push_str(&format!(
        "  Bot token:        {}\n",
        if summary.token_present {
            "present"
        } else {
            "missing"
        }
    ));
    let dm_access = match (&summary.allowed_dm_username, summary.bound_dm_user_id) {
        (None, _) => "open (no restriction)".to_string(),
        (Some(username), None) => format!(
            "restricted to @{} (unbound — first matching message will bind)",
            username
        ),
        (Some(username), Some(_)) => format!("restricted to @{} (bound)", username),
    };
    out.push_str(&format!("  DM access:        {}\n", dm_access));

    out.push_str("\nOptions:\n");
    out.push_str(&format!(
        "  Tool authoring:   {}\n",
        if summary.tool_authoring { "on" } else { "off" }
    ));
    out.push_str(&format!("  Max steps:        {}\n", summary.max_steps));
    out.push_str(&format!("  Max retries:      {}\n", summary.max_retries));
    out.push_str(&format!("  Timeout:          {}s\n", summary.timeout_secs));

    out
}

fn run_one_shot(params: CliParams, instruction: String) -> Result<()> {
    // resolve_one_shot_config is the single validated runtime-contract owner for
    // one-shot runs: workspace, model, API key, and options are all resolved and
    // validated there so this function never re-derives them ad hoc.
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let config = resolve_one_shot_config(params, persisted_defaults)?;
    let workspace = config.workspace;
    let route = config.route;
    let api_key = config.api_key;
    let options = config.options;
    let configured_default_model = config.configured_default_model;

    let cancel_token = CancellationToken::new();
    let interactive_approvals = io::stdin().is_terminal() && io::stderr().is_terminal();
    let approval_mailbox = build_cli_approval_mailbox(interactive_approvals);
    let mut ctx = ExecutionContext::new(workspace.clone())
        .with_cancel_token(cancel_token.clone())
        .with_approval_mailbox(approval_mailbox)
        .with_workspace_checkpoint_store(WorkspaceCheckpointStore::new(workspace));
    let workspace_memory = prepare_workspace_memory(ctx.workspace_root.clone());
    let prepared_run = prepare_run_context(&ctx, &workspace_memory, &instruction, None);
    let loaded_procedure_files = prepared_run.loaded_procedure_files.clone();
    ctx = prepared_run.run_ctx;

    if configured_default_model != route.model_id {
        info!(
            "--model override active; configured default: {}",
            configured_default_model
        );
    }
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
    use crate::config::{TelegramModeDefaults, TOPAGENT_WORKSPACE_KEY};
    use std::collections::HashMap;
    use std::io::Cursor;
    use tempfile::TempDir;
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
    fn test_render_contract_summary_shows_fields_without_secret_values() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            (
                "OPENROUTER_API_KEY".to_string(),
                "sk-real-secret".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "123456:token-secret".to_string(),
            ),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "operator".to_string(),
            ),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);
        let output = render_contract_summary(&summary);

        // Must show key structural fields
        assert!(output.contains("Provider:"), "must show provider: {output}");
        assert!(output.contains("Model:"), "must show model: {output}");
        assert!(
            output.contains("Workspace:"),
            "must show workspace: {output}"
        );
        assert!(
            output.contains("OpenRouter:"),
            "must show OpenRouter key status: {output}"
        );
        assert!(
            output.contains("Bot token:"),
            "must show token status: {output}"
        );
        assert!(
            output.contains("DM access:"),
            "must show DM access: {output}"
        );

        // Must show present/missing, not actual values
        assert!(
            output.contains("present"),
            "must indicate key present: {output}"
        );
        assert!(
            !output.contains("sk-real-secret"),
            "must not reveal OpenRouter key: {output}"
        );
        assert!(
            !output.contains("token-secret"),
            "must not reveal Telegram token: {output}"
        );
        assert!(
            output.contains("operator"),
            "username is safe to show: {output}"
        );
    }

    #[test]
    fn test_render_contract_summary_shows_override_and_default_when_different() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::TOPAGENT_MODEL_KEY.to_string(),
                "persisted/model".to_string(),
            ),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: Some("override/model".to_string()),
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);
        let output = render_contract_summary(&summary);

        assert!(
            output.contains("override/model"),
            "must show effective (overridden) model: {output}"
        );
        assert!(
            output.contains("persisted/model"),
            "must show configured default model: {output}"
        );
        assert!(
            output.contains("CLI override"),
            "must label the source: {output}"
        );
    }

    #[test]
    fn test_render_contract_summary_dm_access_shows_admission_state() {
        let workspace = TempDir::new().unwrap();

        // Unbound: username set, no bound ID
        let values_unbound = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "alice".to_string(),
            ),
        ]);
        let defaults_unbound = TelegramModeDefaults::from_metadata(&values_unbound);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let output_unbound =
            render_contract_summary(&resolve_contract_summary(&params, &defaults_unbound));
        assert!(
            output_unbound.contains("unbound"),
            "must say unbound before first message: {output_unbound}"
        );

        // Bound: username + bound ID
        let mut values_bound = values_unbound.clone();
        values_bound.insert(
            crate::config::TELEGRAM_BOUND_DM_USER_ID_KEY.to_string(),
            "424242".to_string(),
        );
        let defaults_bound = TelegramModeDefaults::from_metadata(&values_bound);
        let output_bound =
            render_contract_summary(&resolve_contract_summary(&params, &defaults_bound));
        assert!(
            output_bound.contains("bound"),
            "must say bound after first message: {output_bound}"
        );
        assert!(
            !output_bound.contains("424242"),
            "must not reveal numeric bound ID: {output_bound}"
        );
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

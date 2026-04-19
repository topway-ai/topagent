// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod checkpoint;
mod commands;
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

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::checkpoint::run_checkpoint_command;
use crate::commands::{run_config_inspect, run_one_shot, run_session_status};
use crate::config::CliParams;
use crate::doctor::run_doctor;
use crate::learning::{
    run_memory_command, run_observation_command, run_procedure_command, run_trajectory_command,
};
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
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

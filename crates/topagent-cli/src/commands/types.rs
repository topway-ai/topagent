use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ToolAuthoringMode {
    On,
    Off,
}

impl ToolAuthoringMode {
    pub(crate) fn is_enabled(self) -> bool {
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
pub(crate) struct Cli {
    #[arg(
        long,
        global = true,
        help = "OpenRouter API key (or OPENROUTER_API_KEY); use --opencode-api-key for Opencode"
    )]
    pub(crate) api_key: Option<String>,

    #[arg(long, global = true, help = "Opencode API key (or OPENCODE_API_KEY)")]
    pub(crate) opencode_api_key: Option<String>,

    #[arg(
        long,
        global = true,
        help = "Model to use (overrides the default model)"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long = "workspace",
        global = true,
        help = "Workspace directory override"
    )]
    pub(crate) workspace: Option<PathBuf>,

    #[arg(long, global = true, help = "Maximum steps for the agent loop")]
    pub(crate) max_steps: Option<usize>,

    #[arg(long, global = true, help = "Maximum provider retries")]
    pub(crate) max_retries: Option<usize>,

    #[arg(long, global = true, help = "Provider timeout in seconds")]
    pub(crate) timeout_secs: Option<u64>,

    #[arg(
        long = "tool-authoring",
        global = true,
        value_enum,
        help = "Enable or disable generated-tool authoring tools for this run or installed service"
    )]
    pub(crate) tool_authoring: Option<ToolAuthoringMode>,

    #[command(subcommand)]
    pub(crate) command: Option<Commands>,

    #[arg(help = "Run a one-shot task")]
    pub(crate) instruction: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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

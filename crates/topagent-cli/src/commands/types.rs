use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::commands::surface::{
    HELP_CONFIG_INSPECT, HELP_DOCTOR, HELP_MEMORY_LINT, HELP_MEMORY_RECALL, HELP_MEMORY_STATUS,
    HELP_MODEL_LIST, HELP_MODEL_PICK, HELP_MODEL_REFRESH, HELP_MODEL_SET, HELP_MODEL_STATUS,
    HELP_PROCEDURE_DISABLE, HELP_PROCEDURE_LIST, HELP_PROCEDURE_PRUNE, HELP_PROCEDURE_SHOW,
    HELP_RUN_DIFF, HELP_RUN_RESTORE, HELP_RUN_STATUS, HELP_STATUS, HELP_TRAJECTORY_EXPORT,
    HELP_TRAJECTORY_LIST, HELP_TRAJECTORY_REVIEW, HELP_TRAJECTORY_SHOW,
};

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
    #[command(about = HELP_STATUS)]
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
    #[command(about = HELP_CONFIG_INSPECT)]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    #[command(about = HELP_DOCTOR)]
    Doctor,
    /// Inspect execution-session state or restore the latest workspace checkpoint.
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
    #[command(about = HELP_RUN_STATUS)]
    Status,
    #[command(about = HELP_RUN_DIFF)]
    Diff,
    #[command(about = HELP_RUN_RESTORE)]
    Restore,
}

#[derive(Subcommand)]
pub(crate) enum ServiceCommands {
    /// Install and start the Telegram background service.
    Install {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
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
    #[command(about = HELP_MODEL_STATUS)]
    Status,
    #[command(about = HELP_MODEL_SET)]
    Set { model_id: String },
    #[command(about = HELP_MODEL_PICK)]
    Pick,
    #[command(about = HELP_MODEL_LIST)]
    List,
    #[command(about = HELP_MODEL_REFRESH)]
    Refresh,
}

/// Internal checkpoint subcommand type used by `run diff` and `run restore`.
pub(crate) enum CheckpointCommands {
    Diff,
    Restore,
}

#[derive(Subcommand)]
pub(crate) enum MemoryCommands {
    #[command(about = HELP_MEMORY_STATUS)]
    Status,
    #[command(about = HELP_MEMORY_LINT)]
    Lint,
    #[command(about = HELP_MEMORY_RECALL)]
    Recall {
        #[arg(help = "Instruction to test recall for")]
        instruction: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProcedureCommands {
    #[command(about = HELP_PROCEDURE_LIST)]
    List {
        #[arg(long, help = "Include superseded and disabled procedures")]
        all: bool,
    },
    #[command(about = HELP_PROCEDURE_SHOW)]
    Show { id: String },
    #[command(about = HELP_PROCEDURE_PRUNE)]
    Prune,
    #[command(about = HELP_PROCEDURE_DISABLE)]
    Disable {
        id: String,
        #[arg(long, help = "Reason for disabling the procedure")]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum TrajectoryCommands {
    #[command(about = HELP_TRAJECTORY_LIST)]
    List,
    #[command(about = HELP_TRAJECTORY_SHOW)]
    Show { id: String },
    #[command(about = HELP_TRAJECTORY_REVIEW)]
    Review { id: String },
    #[command(about = HELP_TRAJECTORY_EXPORT)]
    Export { id: String },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    /// Show the fully resolved runtime contract: provider, model, API key presence,
    /// workspace, Telegram admission state, and runtime options. Never prints
    /// secret values — keys and tokens are shown as present/missing only.
    Inspect,
}

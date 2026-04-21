pub(crate) const PRODUCT_NAME: &str = "TopAgent";

pub(crate) const HELP_STATUS: &str = "Show TopAgent setup, service, and model status.";
pub(crate) const HELP_DOCTOR: &str =
    "Run health diagnostics on setup, config, workspace, and tools.";
pub(crate) const HELP_CONFIG_INSPECT: &str =
    "Show the resolved runtime contract (provider, model, keys, workspace, options).";
pub(crate) const HELP_RUN_STATUS: &str =
    "Show execution-session state: checkpoint, transcripts, and recovery guidance.";
pub(crate) const HELP_RUN_DIFF: &str =
    "Preview the diff between the latest checkpoint and the current workspace.";
pub(crate) const HELP_RUN_RESTORE: &str =
    "Restore the latest checkpoint and clear persisted Telegram transcripts.";

pub(crate) const HELP_MODEL_STATUS: &str = "Show the configured default and effective model.";
pub(crate) const HELP_MODEL_SET: &str =
    "Set the configured model (does not change provider; to change provider, re-run setup).";
pub(crate) const HELP_MODEL_PICK: &str = "Pick the configured model interactively.";
pub(crate) const HELP_MODEL_LIST: &str = "Show the cached model list.";
pub(crate) const HELP_MODEL_REFRESH: &str = "Refresh the cached model list.";

pub(crate) const HELP_MEMORY_STATUS: &str = "Show workspace learning artifact status.";
pub(crate) const HELP_MEMORY_LINT: &str =
    "Lint USER.md and MEMORY.md for size, format, and content policy issues.";
pub(crate) const HELP_MEMORY_RECALL: &str =
    "Dry-run memory retrieval for an instruction and show recall provenance.";

pub(crate) const HELP_PROCEDURE_LIST: &str = "List saved procedures.";
pub(crate) const HELP_PROCEDURE_SHOW: &str = "Show one saved procedure.";
pub(crate) const HELP_PROCEDURE_PRUNE: &str = "Remove superseded and disabled procedures.";
pub(crate) const HELP_PROCEDURE_DISABLE: &str = "Mark a procedure disabled.";

pub(crate) const HELP_TRAJECTORY_LIST: &str = "List saved trajectories.";
pub(crate) const HELP_TRAJECTORY_SHOW: &str = "Show one saved trajectory.";
pub(crate) const HELP_TRAJECTORY_REVIEW: &str = "Mark a trajectory ready for export after review.";
pub(crate) const HELP_TRAJECTORY_EXPORT: &str = "Export a reviewed trajectory.";

pub(crate) const UNINSTALL_PRESERVED: &str = "Neither uninstall nor purge removes the workspace directory itself. \
     Delete it manually if needed.";
pub(crate) const UNINSTALL_PURGE_EXTRA: &str =
    "Purge also removes workspace .topagent/ data and the model cache.";

pub(crate) const MODEL_CHANGE_SEMANTICS: &str = "topagent model set and topagent model pick change only the model, not the provider. \
     To change the provider, re-run topagent setup.";

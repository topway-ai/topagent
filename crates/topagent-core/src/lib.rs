pub mod agent;
pub mod approval;
pub mod behavior;
pub mod cancel;
pub mod channel;
pub mod checkpoint;
pub mod command_exec;
pub mod compaction;
pub mod context;
pub mod error;
pub mod external;
pub mod file_util;
pub mod message;
pub mod model;
pub mod openrouter;
pub mod operator_profile;
pub mod plan;
pub mod progress;
pub mod project;
pub mod prompt;
pub mod provenance;
pub mod provider;
mod run_state;
pub mod runtime;
pub mod secrets;
pub mod session;
pub mod task_result;
pub mod tool_genesis;
pub mod tool_spec;
pub mod tools;

pub use agent::{Agent, ExecutionStage};
pub use approval::{
    ApprovalCheck, ApprovalEnforcement, ApprovalEntry, ApprovalMailbox, ApprovalMailboxMode,
    ApprovalPolicy, ApprovalRequest, ApprovalRequestDraft, ApprovalResolveError, ApprovalState,
    ApprovalTriggerKind, ApprovalTriggerRule,
};
pub use behavior::{BashCommandClass, BehaviorContract, RunStateSnapshot};
pub use cancel::CancellationToken;
pub use channel::telegram::{ChannelError, TelegramAdapter, POLL_TIMEOUT_SECS};
pub use checkpoint::{
    WorkspaceCheckpointRestoreReport, WorkspaceCheckpointStatus, WorkspaceCheckpointStore,
};
pub use command_exec::CommandSandboxPolicy;
pub use compaction::{
    CompactionError, CompactionLevel, CompactionOutcome, CompactionRuntimeState,
    TranscriptCompactor,
};
pub use context::ExecutionContext;
pub use error::{Error, Result};
pub use external::{ExternalTool, ExternalToolEffect, ExternalToolRegistry, ExternalToolResult};
pub use message::{Content, Message, Role};
pub use model::ModelRoute;
pub use openrouter::OpenRouterProvider;
pub use operator_profile::{
    load_operator_profile, migrate_legacy_operator_preferences, save_operator_profile,
    user_profile_path, OperatorPreferenceRecord, OperatorProfile, OperatorProfileMigrationReport,
    PreferenceCategory, USER_PROFILE_RELATIVE_PATH,
};
pub use plan::{Plan, TaskMode, TodoItem, TodoStatus};
pub use progress::{ProgressCallback, ProgressKind, ProgressUpdate};
pub use project::{
    get_project_instructions_or_error, load_project_instructions, ProjectInstructionResult,
};
pub use prompt::{BehaviorPromptContext, NO_PI_MD_NOTE, NO_PROJECT_INSTRUCTIONS_NOTE};
pub use provenance::{
    classify_operator_instruction, fetched_content_source, DurablePromotionKind, InfluenceMode,
    RunTrustContext, SourceKind, SourceLabel, TrustLevel,
};
pub use provider::{Provider, ProviderResponse, ScriptedProvider, ToolCallEntry};
pub use runtime::RuntimeOptions;
pub use secrets::SecretRegistry;
pub use session::Session;
pub use task_result::{TaskEvidence, TaskResult, ToolTraceStep, VerificationCommand};
pub use tool_spec::ToolSpec;

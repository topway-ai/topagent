pub mod agent;
pub mod approval;
pub mod behavior;
pub mod cancel;
pub mod capability;
pub mod channel;
pub mod command_exec;
pub mod compaction;
pub mod context;
pub mod error;
pub mod file_util;
pub mod harness;
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
pub mod run_snapshot;
mod run_state;
pub mod runtime;
pub mod secrets;
pub mod session;
pub mod skills;
pub mod task_result;
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
pub use capability::{
    assess_computer_action, assess_shell_command, is_secret_target, redact_sensitive_target,
    AccessConfig, AccessConfigDocument, AccessMode, AuditEvent, CapabilityApprovalDraft,
    CapabilityApprovalRequest, CapabilityAuditLog, CapabilityAuditRecord, CapabilityDecision,
    CapabilityDecisionDetail, CapabilityError, CapabilityGrant, CapabilityKind, CapabilityManager,
    CapabilityProfile, CapabilityRequest, GrantScope, RiskLevel, ShellAssessment,
};
pub use channel::telegram::{ChannelError, TelegramAdapter, POLL_TIMEOUT_SECS};
pub use command_exec::CommandSandboxPolicy;
pub use compaction::{
    CompactionError, CompactionLevel, CompactionOutcome, CompactionRuntimeState,
    TranscriptCompactor,
};
pub use context::ExecutionContext;
pub use error::{Error, Result};
pub use harness::{AgentHarness, AgentPhase, ContextBundle, SkillDispatcher, SkillExecution};
pub use message::{Content, Message, Role};
pub use model::{
    ModelRoute, ProviderKind, DEFAULT_OPENCODE_MODEL_ID, DEFAULT_OPENROUTER_MODEL_ID,
    OPENCODE_BASE_URL, OPENROUTER_BASE_URL,
};
pub use openrouter::OpenRouterProvider;
pub use operator_profile::{
    load_operator_profile, save_operator_profile, user_profile_path, OperatorPreferenceRecord,
    OperatorProfile, PreferenceCategory, USER_PROFILE_RELATIVE_PATH,
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
pub use run_snapshot::{
    WorkspaceRunSnapshotRestoreReport, WorkspaceRunSnapshotStatus, WorkspaceRunSnapshotStore,
};
pub use runtime::RuntimeOptions;
pub use secrets::SecretRegistry;
pub use session::Session;
pub use skills::{
    default_effects_for_skill, Skill, SkillContext, SkillEffect, SkillEffects, SkillInput,
    SkillOutput, SkillRegistry, SkillResult, SkillSchema, ToolBackedSkill,
};
pub use task_result::{
    DeliveryOutcome, ExecutionSessionOutcome, TaskEvidence, TaskResult, ToolTraceStep,
    VerificationCommand,
};
pub use tool_spec::ToolSpec;

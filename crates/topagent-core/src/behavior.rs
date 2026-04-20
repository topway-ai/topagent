mod action;
mod approval_policy;
mod compaction_policy;
mod durability;
mod task;

use crate::approval::ApprovalPolicy;
use crate::runtime::RuntimeOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorContract {
    pub identity: ProductIdentityPolicy,
    pub task: TaskPolicy,
    pub planning: PlanningPolicy,
    pub tools: ToolPolicy,
    pub mutation: MutationPolicy,
    pub approval: ApprovalPolicy,
    pub output: OutputPolicy,
    pub memory: MemoryPolicy,
    pub generated_tools: GeneratedToolPolicy,
    pub compaction: CompactionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductIdentityPolicy {
    pub primary_channels: &'static [&'static str],
    pub execution_model: &'static str,
    pub scope: &'static str,
    pub operator_model: &'static str,
    pub provider_default: &'static str,
    pub strengths: &'static [&'static str],
    pub non_goals: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPolicy {
    pub direct_instruction_length_threshold: usize,
    pub explicit_plan_phrases: &'static [&'static str],
    pub broad_scope_phrases: &'static [&'static str],
    pub trivial_query_prefixes: &'static [&'static str],
    pub mutation_intent_cues: &'static [&'static str],
    pub classification_system_prompt: &'static str,
    pub task_mode_system_prompt: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningPolicy {
    pub require_plan_by_default: bool,
    pub workflow: &'static [&'static str],
    pub max_blocked_mutations_before_auto_plan: usize,
    pub max_research_steps_without_plan: usize,
    pub max_text_redirects_before_auto_plan: usize,
    pub unplanned_mutation_escalation_threshold: usize,
    pub require_execution_before_verification: bool,
    pub redirect_message: &'static str,
    pub plan_generation_system_prompt: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPolicy {
    pub repo_awareness_tools: &'static [&'static str],
    pub planning_tools: &'static [&'static str],
    pub memory_write_tools: &'static [&'static str],
    pub generated_tool_authoring_tools: &'static [&'static str],
    pub research_safe_bash_prefixes: &'static [&'static str],
    pub verification_bash_prefixes: &'static [&'static str],
    pub verification_bash_keywords: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationPolicy {
    pub mutation_tools: &'static [&'static str],
    pub generated_tool_surface_tools: &'static [&'static str],
    pub destructive_shell_tokens: &'static [&'static str],
    pub shell_write_tokens: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPolicy {
    pub concise_final_response: bool,
    pub avoid_replaying_raw_tool_output: bool,
    pub proof_of_work_for_mutations: bool,
    pub proof_of_work_for_verification: bool,
    pub show_verification_evidence_when_requested: bool,
    pub include_unresolved_issues: bool,
    pub include_workspace_warnings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPolicy {
    pub loaded_memory_is_advisory: bool,
    pub durable_write_tools: &'static [&'static str],
    pub current_state_wins: bool,
    pub never_store: &'static [&'static str],
    pub keep_index_tiny: bool,
    pub index_is_pointer_only: bool,
    pub topic_file_relative_dir: &'static str,
    pub archival_relative_dirs: &'static [&'static str],
    pub index_entry_format: &'static str,
    pub max_index_entries: usize,
    pub max_index_note_chars: usize,
    pub max_index_prompt_bytes: usize,
    pub max_durable_file_prompt_bytes: usize,
    pub max_topics_to_load: usize,
    pub max_transcript_prompt_bytes: usize,
    pub max_transcript_snippets: usize,
    pub max_transcript_message_bytes: usize,
    pub max_curated_lessons: usize,
    pub max_curated_procedures: usize,
    pub max_procedures_to_load: usize,
    pub max_operator_preferences_to_load: usize,
    pub max_operator_prompt_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedToolPolicy {
    pub authoring_enabled: bool,
    pub verified_tools_only: bool,
    pub disposable: bool,
    pub expose_unavailable_warnings: bool,
    pub max_runtime_warning_lines: usize,
    pub reload_after_surface_mutation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPolicy {
    pub micro_trigger_messages: usize,
    pub max_messages_before_truncation: usize,
    pub keep_recent_divisor: usize,
    pub max_compacted_trace_lines: usize,
    pub max_recent_approval_decisions: usize,
    pub max_recent_proof_of_work_anchors: usize,
    pub max_failed_auto_compactions: usize,
    pub refresh_system_prompt_each_turn: bool,
    pub preserved_sections: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunStateSnapshot {
    pub objective: Option<String>,
    pub blockers: Vec<String>,
    pub pending_approvals: Vec<String>,
    pub recent_approval_decisions: Vec<String>,
    pub active_files: Vec<String>,
    pub proof_of_work_anchors: Vec<String>,
    pub trust_notes: Vec<String>,
    pub hook_notes: Vec<String>,
    pub memory_context_loaded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashCommandClass {
    ResearchSafe,
    MutationRisk,
    Verification,
}

pub struct PreExecutionState {
    pub planning_required_for_task: bool,
    pub plan_exists: bool,
    pub execution_started: bool,
    pub task_mode: crate::plan::TaskMode,
}

impl Default for BehaviorContract {
    fn default() -> Self {
        Self::from_runtime_options(&RuntimeOptions::default())
    }
}

impl BehaviorContract {
    pub fn from_runtime_options(options: &RuntimeOptions) -> Self {
        Self {
            identity: default_identity_policy(),
            task: task::default_task_policy(),
            planning: task::default_planning_policy(options),
            tools: action::default_tool_policy(),
            mutation: action::default_mutation_policy(),
            approval: approval_policy::default_approval_policy(),
            output: durability::default_output_policy(),
            memory: durability::default_memory_policy(),
            generated_tools: durability::default_generated_tool_policy(options),
            compaction: compaction_policy::default_compaction_policy(options),
        }
    }
}

pub(crate) fn default_task_policy() -> TaskPolicy {
    task::default_task_policy()
}

pub(crate) fn default_planning_policy(options: &RuntimeOptions) -> PlanningPolicy {
    task::default_planning_policy(options)
}

pub(crate) fn default_memory_policy() -> MemoryPolicy {
    durability::default_memory_policy()
}

fn default_identity_policy() -> ProductIdentityPolicy {
    ProductIdentityPolicy {
        primary_channels: &["Telegram", "CLI"],
        execution_model: "local-first coding agent operating inside the current workspace",
        scope: "repo/workspace-scoped rather than a generic remote assistant",
        operator_model: "operator-centric; keep the user in control of risky actions",
        provider_default: "OpenRouter or Opencode, configured by operator",
        strengths: &[
            "planning",
            "execution",
            "verification",
            "proof-of-work",
            "secret safety",
        ],
        non_goals: &[
            "generic agent framework behavior",
            "multi-agent swarm orchestration in this pass",
        ],
    }
}

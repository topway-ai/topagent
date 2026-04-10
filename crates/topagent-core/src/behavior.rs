use crate::approval::{
    ApprovalEnforcement, ApprovalPolicy, ApprovalRequestDraft, ApprovalTriggerKind,
    ApprovalTriggerRule,
};
use crate::command_exec::CommandSandboxPolicy;
use crate::external::ExternalToolEffect;
use crate::plan::TaskMode;
use crate::provenance::{DurablePromotionKind, RunTrustContext};
use crate::runtime::RuntimeOptions;

const CLASSIFICATION_SYSTEM_PROMPT: &str = "\
You are a task classifier for TopAgent. Given a user instruction, decide \
whether it needs upfront planning before execution.

Respond with ONLY the word \"direct\" or \"plan\". Nothing else.

\"direct\" — the task can be executed immediately:
  - Small edits to one or two files
  - Adding/removing a comment, line, function, or small feature
  - Fixing a typo or small bug
  - Running a verification command
  - Any task the user describes as tiny, small, or simple
  - Tasks that ask for a report or diff after a small change

\"plan\" — the task needs research and planning first:
  - Broad refactors affecting many files
  - Architectural changes
  - Tasks spanning multiple unrelated subsystems
  - Tasks where the user explicitly asks for a plan
  - Large feature implementations with unclear scope";

const TASK_MODE_CLASSIFICATION_SYSTEM_PROMPT: &str = "\
You are a task-mode classifier for TopAgent. Given a user instruction, decide \
what kind of task it is.

Respond with ONLY one of these exact words:
- execute
- inspect
- verify

execute:
  - The task expects the agent to make or apply changes before finishing
  - The task asks to implement, fix, edit, add, remove, refactor, or otherwise mutate something

inspect:
  - The task expects research, analysis, explanation, reporting, or findings only
  - The task should finish without making changes

verify:
  - The task expects running checks or tests only
  - The task may report verification results, but should finish without making changes";

const PLAN_GENERATION_SYSTEM_PROMPT: &str = "\
You are a planning assistant for TopAgent. Given a task, produce a short \
execution plan as a numbered list. Each step should be a concrete action the \
agent can take (read, edit, create, run, verify). Keep the plan short \
(3-7 steps). Do not include preamble or commentary, only the numbered list.";

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
    pub max_curated_plans: usize,
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
    pub task_mode: TaskMode,
}

impl Default for BehaviorContract {
    fn default() -> Self {
        Self::from_runtime_options(&RuntimeOptions::default())
    }
}

impl BehaviorContract {
    pub fn from_runtime_options(options: &RuntimeOptions) -> Self {
        Self {
            identity: ProductIdentityPolicy {
                primary_channels: &["Telegram", "CLI"],
                execution_model: "local-first coding agent operating inside the current workspace",
                scope: "repo/workspace-scoped rather than a generic remote assistant",
                operator_model: "operator-centric; keep the user in control of risky actions",
                provider_default: "OpenRouter-first unless runtime configuration overrides it",
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
            },
            task: TaskPolicy {
                direct_instruction_length_threshold: 120,
                explicit_plan_phrases: &[
                    "make a plan",
                    "create a plan",
                    "give me steps",
                    "give me a checklist",
                    "break down",
                    "step by step",
                ],
                broad_scope_phrases: &[
                    "entire repo",
                    "entire repository",
                    "whole repo",
                    "whole repository",
                    "whole project",
                    "entire project",
                    "project-wide",
                    "across the repo",
                    "across the project",
                    "throughout the",
                    "throughout the repo",
                    "throughout the project",
                    "codebase",
                ],
                trivial_query_prefixes: &[
                    "what is", "where is", "how do", "how does", "show me", "list ", "find ",
                    "search ", "get ", "read ",
                ],
                mutation_intent_cues: &[
                    "fix",
                    "change",
                    "modify",
                    "edit",
                    "write",
                    "update",
                    "implement",
                    "add",
                    "remove",
                    "delete",
                    "refactor",
                    "rename",
                    "create",
                    "patch",
                    "replace",
                ],
                classification_system_prompt: CLASSIFICATION_SYSTEM_PROMPT,
                task_mode_system_prompt: TASK_MODE_CLASSIFICATION_SYSTEM_PROMPT,
            },
            planning: PlanningPolicy {
                require_plan_by_default: options.require_plan,
                workflow: &["Research", "Plan", "Build"],
                max_blocked_mutations_before_auto_plan: 5,
                max_research_steps_without_plan: 10,
                max_text_redirects_before_auto_plan: 2,
                unplanned_mutation_escalation_threshold: 3,
                require_execution_before_verification: true,
                redirect_message: "\
This task requires a plan before proceeding. \
Use the update_plan tool to create a plan with concrete steps, then execute it.",
                plan_generation_system_prompt: PLAN_GENERATION_SYSTEM_PROMPT,
            },
            tools: ToolPolicy {
                repo_awareness_tools: &["git_status", "git_branch", "git_diff"],
                planning_tools: &["update_plan", "save_plan"],
                memory_write_tools: &[
                    "save_plan",
                    "save_lesson",
                    "manage_operator_preference",
                ],
                generated_tool_authoring_tools: &[
                    "create_tool",
                    "repair_tool",
                    "list_generated_tools",
                    "delete_generated_tool",
                ],
                research_safe_bash_prefixes: &[
                    "cd ",
                    "pushd ",
                    "popd",
                    "ls ",
                    "ls-",
                    "pwd",
                    "find ",
                    "find -",
                    "rg ",
                    "rg -",
                    "grep ",
                    "grep -",
                    "cat ",
                    "head ",
                    "tail ",
                    "wc ",
                    "cut ",
                    "sort ",
                    "uniq ",
                    "diff ",
                    "git status",
                    "git diff",
                    "git log ",
                    "git show",
                    "git blame",
                    "git branch",
                    "git remote",
                    "git stash list",
                    "echo ",
                    "printf ",
                    "true",
                    "false",
                ],
                verification_bash_prefixes: &[
                    "cargo test",
                    "cargo build",
                    "cargo check",
                    "cargo clippy",
                    "cargo fmt",
                    "cargo watch",
                    "cargo auditable",
                    "cargo deny",
                    "cargo audit",
                    "pytest",
                    "py.test",
                    "make test",
                    "make check",
                    "make verify",
                    "npm test",
                    "npm run test",
                    "npm run build",
                    "npm run check",
                    "go test",
                    "go build",
                    "go vet",
                    "rustfmt",
                    "rust-analyzer",
                    "clippy",
                    "deny ",
                    "audit ",
                ],
                verification_bash_keywords: &[
                    "test", "build", "check", "lint", "fmt", "audit", "vet",
                ],
            },
            mutation: MutationPolicy {
                mutation_tools: &["write", "edit", "git_commit", "git_add"],
                generated_tool_surface_tools: &[
                    "create_tool",
                    "repair_tool",
                    "delete_generated_tool",
                ],
                destructive_shell_tokens: &["rm ", "mv ", "cp ", "touch ", "mkdir "],
                shell_write_tokens: &[" >", ">>", "|"],
            },
            approval: ApprovalPolicy {
                mailbox_available: false,
                triggers: &[
                    ApprovalTriggerRule {
                        kind: ApprovalTriggerKind::GitCommit,
                        label: "git_commit",
                        enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                        rationale: "commits publish a durable repo milestone",
                    },
                    ApprovalTriggerRule {
                        kind: ApprovalTriggerKind::DestructiveShellMutation,
                        label: "shell mutation",
                        enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                        rationale: "shell mutations can bypass safer structured tools",
                    },
                    ApprovalTriggerRule {
                        kind: ApprovalTriggerKind::HostExternalExecution,
                        label: "host-sandbox external tool execution",
                        enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                        rationale: "host tools reach beyond the workspace sandbox",
                    },
                    ApprovalTriggerRule {
                        kind: ApprovalTriggerKind::GeneratedToolDeletion,
                        label: "delete_generated_tool",
                        enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                        rationale: "tool deletion removes workspace-local operator tooling",
                    },
                ],
            },
            output: OutputPolicy {
                concise_final_response: true,
                avoid_replaying_raw_tool_output: true,
                proof_of_work_for_mutations: true,
                proof_of_work_for_verification: true,
                show_verification_evidence_when_requested: true,
                include_unresolved_issues: true,
                include_workspace_warnings: true,
            },
            memory: MemoryPolicy {
                loaded_memory_is_advisory: true,
                durable_write_tools: &[
                    "save_plan",
                    "save_lesson",
                    "manage_operator_preference",
                ],
                current_state_wins: true,
                never_store: &[
                    "transcripts",
                    "logs",
                    "command-output dumps",
                    "transient plans",
                    "secrets",
                ],
                keep_index_tiny: true,
                index_is_pointer_only: true,
                topic_file_relative_dir: "topics",
                archival_relative_dirs: &["lessons", "plans", "procedures"],
                index_entry_format:
                    "- topic: <name> | file: topics/<name>.md | status: verified|tentative|stale | tags: tag1, tag2 | note: short pointer",
                max_index_entries: 24,
                max_index_note_chars: 120,
                max_index_prompt_bytes: 1_400,
                max_durable_file_prompt_bytes: 1_200,
                max_topics_to_load: 2,
                max_transcript_prompt_bytes: 1_500,
                max_transcript_snippets: 3,
                max_transcript_message_bytes: 220,
                max_curated_lessons: 6,
                max_curated_plans: 4,
                max_curated_procedures: 4,
                max_procedures_to_load: 2,
                max_operator_preferences_to_load: 2,
                max_operator_prompt_bytes: 600,
            },
            generated_tools: GeneratedToolPolicy {
                authoring_enabled: options.enable_generated_tool_authoring,
                verified_tools_only: true,
                disposable: true,
                expose_unavailable_warnings: true,
                reload_after_surface_mutation: true,
            },
            compaction: CompactionPolicy {
                micro_trigger_messages: std::cmp::max(4, options.max_messages_before_truncation / 2),
                max_messages_before_truncation: options.max_messages_before_truncation,
                keep_recent_divisor: 2,
                max_compacted_trace_lines: 8,
                max_recent_approval_decisions: 3,
                max_recent_proof_of_work_anchors: 4,
                max_failed_auto_compactions: 2,
                refresh_system_prompt_each_turn: true,
                preserved_sections: &[
                    "behavior contract",
                    "current objective",
                    "available tools",
                    "project instructions",
                    "workspace memory",
                    "generated tool warnings",
                    "current plan",
                    "blockers",
                    "pending approvals",
                    "approval decisions",
                    "active files",
                    "proof-of-work anchors",
                ],
            },
        }
    }

    pub fn classify_task_fast_path(&self, instruction: &str) -> Option<bool> {
        let lower = instruction.to_lowercase();

        if self
            .task
            .explicit_plan_phrases
            .iter()
            .any(|phrase| lower.contains(phrase))
        {
            return Some(true);
        }

        if self
            .task
            .broad_scope_phrases
            .iter()
            .any(|phrase| lower.contains(phrase))
        {
            return Some(true);
        }

        if self
            .task
            .trivial_query_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            && lower.len() < self.task.direct_instruction_length_threshold
        {
            return Some(false);
        }

        if lower.len() <= self.task.direct_instruction_length_threshold {
            return Some(false);
        }

        None
    }

    pub fn build_task_classification_messages(&self, instruction: &str) -> (String, String) {
        (
            self.task.classification_system_prompt.to_string(),
            instruction.to_string(),
        )
    }

    pub fn task_mode_fast_path(&self, instruction: &str) -> Option<TaskMode> {
        let lower = instruction.to_lowercase();
        self.task
            .mutation_intent_cues
            .iter()
            .any(|cue| lower.contains(cue))
            .then_some(TaskMode::PlanAndExecute)
    }

    pub fn build_task_mode_messages(&self, instruction: &str) -> (String, String) {
        (
            self.task.task_mode_system_prompt.to_string(),
            instruction.to_string(),
        )
    }

    pub fn build_plan_generation_prompt(&self, instruction: &str) -> (String, String) {
        (
            self.planning.plan_generation_system_prompt.to_string(),
            format!("Create a plan for this task:\n\n{instruction}"),
        )
    }

    fn is_research_safe_command(&self, cmd: &str) -> bool {
        let lower = cmd.trim().to_lowercase();
        self.tools
            .research_safe_bash_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix) || lower == prefix.trim_end_matches(' '))
    }

    fn has_file_write_redirection(&self, cmd: &str) -> bool {
        let mut chars = cmd.char_indices().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some((_, ch)) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' if !in_single => {
                    escaped = true;
                }
                '\'' if !in_double => {
                    in_single = !in_single;
                }
                '"' if !in_single => {
                    in_double = !in_double;
                }
                '>' if !in_single && !in_double => {
                    if chars.peek().is_some_and(|(_, next)| *next == '>') {
                        chars.next();
                    }

                    while chars.peek().is_some_and(|(_, next)| next.is_whitespace()) {
                        chars.next();
                    }

                    let mut target = String::new();
                    while let Some((_, next)) = chars.peek() {
                        if next.is_whitespace() || matches!(next, '|' | ';') {
                            break;
                        }
                        target.push(*next);
                        chars.next();
                    }

                    if target.is_empty() || target.starts_with('&') || target == "/dev/null" {
                        continue;
                    }

                    return true;
                }
                _ => {}
            }
        }

        false
    }

    fn contains_mutation_signal(&self, cmd: &str) -> bool {
        let lower = cmd.trim().to_lowercase();
        self.has_file_write_redirection(cmd)
            || self
                .mutation
                .destructive_shell_tokens
                .iter()
                .any(|token| lower.contains(token))
            || lower.contains(" -delete")
    }

    fn split_shell_segments<'a>(&self, cmd: &'a str) -> Vec<&'a str> {
        let mut segments = Vec::new();
        let mut start = 0;
        let mut chars = cmd.char_indices().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some((idx, ch)) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' if !in_single => {
                    escaped = true;
                }
                '\'' if !in_double => {
                    in_single = !in_single;
                }
                '"' if !in_single => {
                    in_double = !in_double;
                }
                ';' if !in_single && !in_double => {
                    let segment = cmd[start..idx].trim();
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                    start = idx + ch.len_utf8();
                }
                '|' if !in_single && !in_double => {
                    let is_double_pipe = chars.peek().is_some_and(|(_, next)| *next == '|');
                    if is_double_pipe {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        let (_, next) = chars.next().expect("peeked pipe should exist");
                        start = idx + ch.len_utf8() + next.len_utf8();
                    } else {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        start = idx + ch.len_utf8();
                    }
                }
                '&' if !in_single && !in_double => {
                    if chars.peek().is_some_and(|(_, next)| *next == '&') {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        let (_, next) = chars.next().expect("peeked ampersand should exist");
                        start = idx + ch.len_utf8() + next.len_utf8();
                    }
                }
                _ => {}
            }
        }

        let tail = cmd[start..].trim();
        if !tail.is_empty() {
            segments.push(tail);
        }

        segments
    }

    pub fn classify_bash_command(&self, cmd: &str) -> BashCommandClass {
        let trimmed = cmd.trim();

        if self.is_verification_command(trimmed) {
            return BashCommandClass::Verification;
        }

        let mut saw_verification = false;
        for segment in self.split_shell_segments(trimmed) {
            if self.contains_mutation_signal(segment) {
                return BashCommandClass::MutationRisk;
            }

            if self.is_verification_command(segment) {
                saw_verification = true;
                continue;
            }

            if self.is_research_safe_command(segment) {
                continue;
            }

            return BashCommandClass::MutationRisk;
        }

        if saw_verification {
            BashCommandClass::Verification
        } else {
            BashCommandClass::ResearchSafe
        }
    }

    pub fn approval_request(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        bash_command: Option<&str>,
        external_effect: Option<ExternalToolEffect>,
        external_sandbox: Option<CommandSandboxPolicy>,
        trust_context: Option<&RunTrustContext>,
    ) -> Option<ApprovalRequestDraft> {
        let low_trust_summary = trust_context.and_then(|trust| trust.low_trust_action_summary(2));

        if tool_name == "git_commit" {
            let message = args
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing commit message>");
            return self.build_approval_request(
                ApprovalTriggerKind::GitCommit,
                format!("git commit: {}", Self::compact_action_text(message, 80)),
                format!("git_commit(message={message:?})"),
                "Creates a new git commit in the current workspace repository.".to_string(),
                "Staged changes become a durable repo milestone.".to_string(),
                Some("Use git revert or git reset if the commit needs to be undone.".to_string()),
                low_trust_summary.as_deref(),
            );
        }

        if tool_name == "bash" {
            let command = bash_command?;
            if self.classify_bash_command(command) != BashCommandClass::MutationRisk {
                return None;
            }

            return self.build_approval_request(
                ApprovalTriggerKind::DestructiveShellMutation,
                format!(
                    "bash mutation: {}",
                    Self::compact_action_text(command.trim(), 90)
                ),
                command.trim().to_string(),
                "May create, overwrite, move, or delete files outside structured edit tools."
                    .to_string(),
                "Runs a filesystem-changing shell command directly through the shell."
                    .to_string(),
                Some(
                    "Use `topagent checkpoint restore` for the latest workspace checkpoint, then inspect git diff for any remaining shell-side effects."
                        .to_string(),
                ),
                low_trust_summary.as_deref(),
            );
        }

        if external_sandbox == Some(CommandSandboxPolicy::Host) {
            let effect = match external_effect.unwrap_or(ExternalToolEffect::ReadOnly) {
                ExternalToolEffect::ReadOnly => {
                    "Runs a host-scoped external tool outside the workspace sandbox."
                }
                ExternalToolEffect::VerificationOnly => {
                    "Runs a host-scoped verification tool outside the workspace sandbox."
                }
                ExternalToolEffect::ExecutionStarted => {
                    "Runs a host-scoped execution tool outside the workspace sandbox."
                }
            };
            return self.build_approval_request(
                ApprovalTriggerKind::HostExternalExecution,
                format!("host external tool: {tool_name}"),
                format!("{tool_name}({})", Self::compact_json(args)),
                "May reach beyond the workspace sandbox and affect host-visible state.".to_string(),
                effect.to_string(),
                None,
                low_trust_summary.as_deref(),
            );
        }

        if tool_name == "delete_generated_tool" {
            let name = args
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing tool name>");
            return self.build_approval_request(
                ApprovalTriggerKind::GeneratedToolDeletion,
                format!("delete generated tool: {name}"),
                format!("delete_generated_tool(name={name:?})"),
                "Removes a workspace-local tool from .topagent/tools/.".to_string(),
                "Deletes the generated tool until it is recreated.".to_string(),
                Some("Use create_tool or repair_tool to restore the tool later.".to_string()),
                low_trust_summary.as_deref(),
            );
        }

        None
    }

    pub fn memory_write_block_reason(
        &self,
        tool_name: &str,
        trust_context: &RunTrustContext,
        corroborated_by_trusted_local: bool,
    ) -> Option<String> {
        let summary = trust_context.low_trust_action_summary(2)?;

        if tool_name == "manage_operator_preference" {
            return Some(format!(
                "durable operator preference writes are blocked because this run is influenced by low-trust content from: {}. Re-derive the preference from direct operator intent first.",
                summary
            ));
        }

        if self.is_memory_write_tool(tool_name) && !corroborated_by_trusted_local {
            return Some(format!(
                "durable memory writes are blocked because this run is influenced by low-trust content from: {} without trusted workspace corroboration.",
                summary
            ));
        }

        None
    }

    pub fn durable_promotion_block_reason(
        &self,
        kind: DurablePromotionKind,
        trust_context: &RunTrustContext,
        corroborated_by_trusted_local: bool,
    ) -> Option<String> {
        let summary = trust_context.low_trust_action_summary(2)?;

        match kind {
            DurablePromotionKind::Lesson if corroborated_by_trusted_local => None,
            DurablePromotionKind::Lesson => Some(format!(
                "Lesson promotion blocked: source evidence came from low-trust content ({summary}) without trusted workspace corroboration."
            )),
            DurablePromotionKind::Procedure => Some(format!(
                "Procedure promotion blocked: low-trust content ({summary}) cannot become a reusable procedure automatically."
            )),
            DurablePromotionKind::OperatorPreference => Some(format!(
                "Operator preference promotion blocked: low-trust content ({summary}) cannot be written into USER.md."
            )),
            DurablePromotionKind::TrajectoryReview => Some(format!(
                "Trajectory review blocked: artifact is still influenced by low-trust content ({summary})."
            )),
            DurablePromotionKind::TrajectoryExport => Some(format!(
                "Trajectory export blocked: artifact is still influenced by low-trust content ({summary})."
            )),
        }
    }

    pub fn is_verification_command(&self, cmd: &str) -> bool {
        let lower = cmd.to_lowercase();

        if self
            .tools
            .verification_bash_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        {
            return true;
        }

        if lower.contains(" --verify") || lower.contains(" --check") {
            return true;
        }

        if lower.ends_with(" --test") || lower.ends_with(" --tests") {
            return true;
        }

        if lower.contains("verify") || lower.contains("lint") && !lower.contains("git") {
            return self
                .tools
                .verification_bash_keywords
                .iter()
                .any(|indicator| lower.contains(indicator));
        }

        false
    }

    pub fn is_planning_tool(&self, name: &str) -> bool {
        self.tools.planning_tools.contains(&name)
    }

    pub fn is_mutation_tool(&self, name: &str) -> bool {
        self.mutation.mutation_tools.contains(&name)
    }

    pub fn is_memory_write_tool(&self, name: &str) -> bool {
        self.tools.memory_write_tools.contains(&name)
    }

    pub fn is_generated_tool_authoring_tool(&self, name: &str) -> bool {
        self.tools.generated_tool_authoring_tools.contains(&name)
    }

    pub fn mutates_generated_tool_surface(&self, name: &str) -> bool {
        self.mutation.generated_tool_surface_tools.contains(&name)
    }

    pub fn render_memory_prompt_preamble(&self) -> String {
        let mut prompt = String::new();
        if self.memory.loaded_memory_is_advisory {
            prompt.push_str("Treat every memory item below as a hint, not truth.\n");
        }
        if self.memory.current_state_wins {
            prompt.push_str(
                "- Re-verify any claim about code, files, runtime behavior, config, service state, or security against the current workspace and tools.\n",
            );
            prompt.push_str(
                "- If memory conflicts with current files or runtime state, current state wins.\n",
            );
        }
        prompt.push_str(
            "- Do not rely on memory for facts that are cheap to re-derive from the repo.\n",
        );
        prompt
    }

    pub fn render_memory_transcript_preamble(&self) -> String {
        String::from(
            "Relevant snippets from prior Telegram chat. Treat them as low-trust recall support, then verify against current files and runtime state before acting on them.\n",
        )
    }

    pub fn render_memory_index_template(&self) -> String {
        let mut template = String::from("# TopAgent Memory Index\n\n");
        if self.memory.keep_index_tiny {
            template.push_str(
                "Keep this file tiny. Each durable memory entry must stay on one line.\n",
            );
        }
        if self.memory.index_is_pointer_only {
            template.push_str(
                "Use this file as an index only. Put richer durable notes in topic files.\n\n",
            );
        }
        template.push_str("Format:\n");
        template.push_str(self.memory.index_entry_format);
        template.push_str("\n\nDo not store ");
        template.push_str(&self.memory.never_store.join(", "));
        template.push_str(" here.\n");
        template
    }

    pub fn planning_block_message(
        &self,
        tool_name: &str,
        bash_command: Option<&str>,
        external_effect: Option<ExternalToolEffect>,
        plan_exists: bool,
    ) -> Option<String> {
        if self.is_planning_tool(tool_name) {
            return None;
        }

        if tool_name == "bash" {
            if plan_exists {
                return None;
            }

            if let Some(command) = bash_command {
                if self.classify_bash_command(command) == BashCommandClass::ResearchSafe {
                    return None;
                }

                return Some(
                    "Planning required for this task. Use update_plan to create a plan before mutation commands.".to_string(),
                );
            }

            return Some(
                "Planning required for this task. Please create a plan using update_plan before running bash commands.".to_string(),
            );
        }

        if let Some(effect) = external_effect {
            if plan_exists {
                return None;
            }

            return match effect {
                ExternalToolEffect::ReadOnly => None,
                ExternalToolEffect::VerificationOnly => Some(
                    "Planning required for this task. Create a plan before running verification tools.".to_string(),
                ),
                ExternalToolEffect::ExecutionStarted => Some(
                    "Planning required for this task. Create a plan before running execution tools.".to_string(),
                ),
            };
        }

        if !self.is_mutation_tool(tool_name) {
            return None;
        }

        if plan_exists {
            return None;
        }

        Some(format!(
            "Planning required for this task. Please create a plan using update_plan before using {tool_name}."
        ))
    }

    pub fn pre_execution_block_message(
        &self,
        tool_name: &str,
        bash_command: Option<&str>,
        external_effect: Option<ExternalToolEffect>,
        state: &PreExecutionState,
    ) -> Option<String> {
        if !state.planning_required_for_task
            || !state.plan_exists
            || state.execution_started
            || state.task_mode != TaskMode::PlanAndExecute
            || !self.planning.require_execution_before_verification
        {
            return None;
        }

        if tool_name == "bash" {
            let command = bash_command?;
            if self.classify_bash_command(command) == BashCommandClass::Verification {
                return Some(
                    "A plan exists, but no concrete execution step has run yet. Execute at least one plan step before verification commands.".to_string(),
                );
            }
        }

        if matches!(external_effect, Some(ExternalToolEffect::VerificationOnly)) {
            return Some(
                "A plan exists, but no concrete execution step has run yet. Execute at least one plan step before verification tools.".to_string(),
            );
        }

        None
    }

    pub fn should_escalate_to_planning(
        &self,
        planning_gate_active: bool,
        planning_escalated: bool,
        plan_exists: bool,
        changed_file_count: usize,
    ) -> bool {
        self.planning.require_plan_by_default
            && !planning_gate_active
            && !planning_escalated
            && !plan_exists
            && changed_file_count >= self.planning.unplanned_mutation_escalation_threshold
    }

    pub fn should_attach_proof_of_work(
        &self,
        changed_files: usize,
        verification_commands: usize,
        unresolved_issues: usize,
        workspace_warnings: usize,
    ) -> bool {
        changed_files > 0 && self.output.proof_of_work_for_mutations
            || verification_commands > 0 && self.output.proof_of_work_for_verification
            || unresolved_issues > 0 && self.output.include_unresolved_issues
            || workspace_warnings > 0 && self.output.include_workspace_warnings
    }

    pub fn keep_recent_message_count(&self) -> usize {
        std::cmp::max(
            1,
            self.compaction.max_messages_before_truncation / self.compaction.keep_recent_divisor,
        )
    }

    pub fn full_rebuild_recent_message_count(&self) -> usize {
        std::cmp::max(
            8,
            self.compaction.max_messages_before_truncation
                / (self.compaction.keep_recent_divisor * 4),
        )
    }

    pub fn should_micro_compact(&self, message_count: usize) -> bool {
        message_count >= self.compaction.micro_trigger_messages
    }

    pub fn should_auto_compact(&self, message_count: usize) -> bool {
        message_count >= self.compaction.max_messages_before_truncation
    }

    pub fn build_truncation_notice(&self, dropped_count: usize) -> String {
        format!(
            "[Previous {dropped_count} messages truncated due to context length. \
Preserved via fresh system prompt each turn: {}. Use tools to re-read files if you need earlier context.]",
            self.compaction.preserved_sections.join(", ")
        )
    }

    fn build_approval_request(
        &self,
        kind: ApprovalTriggerKind,
        short_summary: String,
        exact_action: String,
        scope_of_impact: String,
        expected_effect: String,
        rollback_hint: Option<String>,
        low_trust_summary: Option<&str>,
    ) -> Option<ApprovalRequestDraft> {
        let rule = self
            .approval
            .triggers
            .iter()
            .find(|rule| rule.kind == kind)?;
        if rule.enforcement == ApprovalEnforcement::AdvisoryOnly {
            return None;
        }
        let reason = match low_trust_summary {
            Some(summary) => format!(
                "{} Proposed action is influenced by low-trust content from: {}.",
                rule.rationale, summary
            ),
            None => rule.rationale.to_string(),
        };
        Some(ApprovalRequestDraft {
            action_kind: kind,
            short_summary,
            exact_action,
            reason,
            scope_of_impact,
            expected_effect,
            rollback_hint,
        })
    }

    fn compact_action_text(text: &str, limit: usize) -> String {
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.len() <= limit {
            compact
        } else {
            format!("{}...", &compact[..limit.saturating_sub(3)])
        }
    }

    fn compact_json(value: &serde_json::Value) -> String {
        let rendered = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
        Self::compact_action_text(&rendered, 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{InfluenceMode, RunTrustContext, SourceKind, SourceLabel};

    #[test]
    fn test_contract_respects_runtime_options() {
        let options = RuntimeOptions::default()
            .with_require_plan(false)
            .with_generated_tool_authoring(true)
            .with_max_messages_before_truncation(42);
        let contract = BehaviorContract::from_runtime_options(&options);

        assert!(!contract.planning.require_plan_by_default);
        assert!(contract.generated_tools.authoring_enabled);
        assert_eq!(contract.compaction.max_messages_before_truncation, 42);
        assert_eq!(contract.compaction.micro_trigger_messages, 21);
    }

    #[test]
    fn test_operator_preference_tool_is_classified_as_memory_write() {
        let contract = BehaviorContract::default();

        assert!(contract.is_memory_write_tool("manage_operator_preference"));
        assert!(contract
            .memory
            .durable_write_tools
            .contains(&"manage_operator_preference"));
    }

    #[test]
    fn test_classify_task_fast_path_matches_current_rules() {
        let contract = BehaviorContract::default();

        assert_eq!(
            contract.classify_task_fast_path("make a plan for the refactor"),
            Some(true)
        );
        assert_eq!(
            contract.classify_task_fast_path("refactor the entire repo"),
            Some(true)
        );
        assert_eq!(
            contract.classify_task_fast_path("read this file"),
            Some(false)
        );
        assert_eq!(
            contract.classify_task_fast_path("fix the typo in main.rs"),
            Some(false)
        );
    }

    #[test]
    fn test_classify_bash_command_routes_expected_classes() {
        let contract = BehaviorContract::default();

        assert_eq!(
            contract.classify_bash_command("git status"),
            BashCommandClass::ResearchSafe
        );
        assert_eq!(
            contract.classify_bash_command("cargo test --lib"),
            BashCommandClass::Verification
        );
        assert_eq!(
            contract.classify_bash_command("echo hi > file.txt"),
            BashCommandClass::MutationRisk
        );
        assert_eq!(
            contract.classify_bash_command("find . -type f 2>/dev/null | head -20"),
            BashCommandClass::ResearchSafe
        );
        assert_eq!(
            contract.classify_bash_command("cd /tmp/topagent && find . -type f | wc -l"),
            BashCommandClass::ResearchSafe
        );
        assert_eq!(
            contract.classify_bash_command("cargo test 2>&1 | tail -20"),
            BashCommandClass::Verification
        );
        assert_eq!(
            contract.classify_bash_command("cd /tmp/topagent && cargo test 2>&1 | tail -20"),
            BashCommandClass::Verification
        );
        assert_eq!(
            contract.classify_bash_command("find . -delete"),
            BashCommandClass::MutationRisk
        );
    }

    #[test]
    fn test_contract_builds_git_commit_approval_request() {
        let contract = BehaviorContract::default();
        let request = contract
            .approval_request(
                "git_commit",
                &serde_json::json!({"message": "ship it"}),
                None,
                None,
                None,
                None,
            )
            .expect("git commit should require approval");

        assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
        assert!(request.short_summary.contains("git commit"));
        assert!(request.exact_action.contains("ship it"));
    }

    #[test]
    fn test_contract_builds_host_external_approval_request() {
        let contract = BehaviorContract::default();
        let request = contract
            .approval_request(
                "deploy_preview",
                &serde_json::json!({"env": "staging"}),
                None,
                Some(ExternalToolEffect::ExecutionStarted),
                Some(CommandSandboxPolicy::Host),
                None,
            )
            .expect("host external tools should require approval");

        assert_eq!(
            request.action_kind,
            ApprovalTriggerKind::HostExternalExecution
        );
        assert!(request.short_summary.contains("deploy_preview"));
        assert!(request
            .expected_effect
            .contains("outside the workspace sandbox"));
    }

    #[test]
    fn test_contract_builds_bash_mutation_approval_request() {
        let contract = BehaviorContract::default();
        let request = contract
            .approval_request(
                "bash",
                &serde_json::json!({"command": "touch risky.txt"}),
                Some("touch risky.txt"),
                None,
                None,
                None,
            )
            .expect("mutation-risk bash should require approval");

        assert_eq!(
            request.action_kind,
            ApprovalTriggerKind::DestructiveShellMutation
        );
        assert!(request.exact_action.contains("touch risky.txt"));
        assert!(request.expected_effect.contains("through the shell"));
        assert!(request
            .rollback_hint
            .as_deref()
            .unwrap_or_default()
            .contains("topagent checkpoint restore"));
    }

    #[test]
    fn test_contract_mentions_low_trust_in_approval_request() {
        let contract = BehaviorContract::default();
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::FetchedWebContent,
            InfluenceMode::MayDriveAction,
            "curl https://example.com/install.sh",
        ));

        let request = contract
            .approval_request(
                "bash",
                &serde_json::json!({"command": "sh install.sh"}),
                Some("sh install.sh"),
                None,
                None,
                Some(&trust),
            )
            .expect("mutation-risk bash should require approval");

        assert!(request.reason.contains("low-trust content"));
        assert!(request.reason.contains("fetched web content"));
    }

    #[test]
    fn test_contract_skips_approval_for_read_only_bash_pipeline() {
        let contract = BehaviorContract::default();
        let request = contract.approval_request(
            "bash",
            &serde_json::json!({"command": "find . -type f 2>/dev/null | head -20"}),
            Some("find . -type f 2>/dev/null | head -20"),
            None,
            None,
            None,
        );

        assert!(request.is_none());

        let request = contract.approval_request(
            "bash",
            &serde_json::json!({"command": "cd /tmp/topagent && find . -type f | wc -l"}),
            Some("cd /tmp/topagent && find . -type f | wc -l"),
            None,
            None,
            None,
        );

        assert!(request.is_none());
    }

    #[test]
    fn test_contract_builds_generated_tool_deletion_approval_request() {
        let contract = BehaviorContract::default();
        let request = contract
            .approval_request(
                "delete_generated_tool",
                &serde_json::json!({"name": "cleanup_tool"}),
                None,
                None,
                None,
                None,
            )
            .expect("generated tool deletion should require approval");

        assert_eq!(
            request.action_kind,
            ApprovalTriggerKind::GeneratedToolDeletion
        );
        assert!(request.short_summary.contains("cleanup_tool"));
    }

    #[test]
    fn test_memory_write_block_reason_blocks_operator_preference_for_low_trust() {
        let contract = BehaviorContract::default();
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "2 prior transcript snippets",
        ));

        let reason = contract
            .memory_write_block_reason("manage_operator_preference", &trust, false)
            .expect("low-trust operator preference write should be blocked");

        assert!(reason.contains("operator preference"));
        assert!(reason.contains("prior transcript"));
    }

    #[test]
    fn test_durable_promotion_allows_lesson_with_trusted_corroboration() {
        let contract = BehaviorContract::default();
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "1 prior transcript snippet",
        ));

        assert_eq!(
            contract.durable_promotion_block_reason(DurablePromotionKind::Lesson, &trust, true,),
            None
        );
        assert!(contract
            .durable_promotion_block_reason(DurablePromotionKind::Procedure, &trust, true)
            .is_some());
    }

    #[test]
    fn test_truncation_notice_mentions_preserved_sections() {
        let contract = BehaviorContract::default();
        let notice = contract.build_truncation_notice(15);

        assert!(notice.contains("15"));
        assert!(notice.contains("behavior contract"));
        assert!(notice.contains("current plan"));
    }

    #[test]
    fn test_render_memory_index_template_uses_contract_policy() {
        let contract = BehaviorContract::default();
        let template = contract.render_memory_index_template();

        assert!(template.contains("Keep this file tiny"));
        assert!(template.contains("Use this file as an index only"));
        assert!(template.contains("Do not store transcripts"));
    }
}

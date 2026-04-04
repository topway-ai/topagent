use crate::external::ExternalToolEffect;
use crate::plan::{Plan, TaskMode};
use crate::runtime::RuntimeOptions;
use crate::ToolSpec;

pub const NO_PROJECT_INSTRUCTIONS_NOTE: &str =
    "\n[Note: No TOPAGENT.md file is present in the workspace root. Do not try to read it.]\n";

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
pub struct ApprovalPolicy {
    pub mailbox_available: bool,
    pub triggers: &'static [ApprovalTriggerRule],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalTriggerRule {
    pub kind: ApprovalTriggerKind,
    pub label: &'static str,
    pub enforcement: ApprovalEnforcement,
    pub rationale: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTriggerKind {
    GitCommit,
    DestructiveShellMutation,
    HostExternalExecution,
    GeneratedToolDeletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalEnforcement {
    AdvisoryOnly,
    RequiredWhenAvailable,
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
    pub max_messages_before_truncation: usize,
    pub keep_recent_divisor: usize,
    pub refresh_system_prompt_each_turn: bool,
    pub preserved_sections: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashCommandClass {
    ResearchSafe,
    MutationRisk,
    Verification,
}

pub struct BehaviorPromptContext<'a> {
    pub available_tools: &'a [ToolSpec],
    pub external_tools: &'a [ToolSpec],
    pub project_instructions: Option<&'a str>,
    pub memory_context: Option<&'a str>,
    pub current_plan: Option<&'a Plan>,
    pub generated_tool_warnings: &'a [String],
    pub planning_required_now: bool,
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
                memory_write_tools: &["save_plan", "save_lesson"],
                generated_tool_authoring_tools: &[
                    "create_tool",
                    "repair_tool",
                    "list_generated_tools",
                    "delete_generated_tool",
                ],
                research_safe_bash_prefixes: &[
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
                        label: "destructive shell mutation",
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
                durable_write_tools: &["save_plan", "save_lesson"],
                current_state_wins: true,
                never_store: &[
                    "transcripts",
                    "logs",
                    "command-output dumps",
                    "transient plans",
                    "secrets",
                ],
            },
            generated_tools: GeneratedToolPolicy {
                authoring_enabled: options.enable_generated_tool_authoring,
                verified_tools_only: true,
                disposable: true,
                expose_unavailable_warnings: true,
                reload_after_surface_mutation: true,
            },
            compaction: CompactionPolicy {
                max_messages_before_truncation: options.max_messages_before_truncation,
                keep_recent_divisor: 2,
                refresh_system_prompt_each_turn: true,
                preserved_sections: &[
                    "behavior contract",
                    "available tools",
                    "project instructions",
                    "workspace memory",
                    "generated tool warnings",
                    "current plan",
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

    pub fn classify_bash_command(&self, cmd: &str) -> BashCommandClass {
        let trimmed = cmd.trim();
        let lower = trimmed.to_lowercase();

        if self.is_verification_command(trimmed) {
            return BashCommandClass::Verification;
        }

        if self
            .mutation
            .shell_write_tokens
            .iter()
            .any(|token| lower.contains(token))
            || self
                .mutation
                .destructive_shell_tokens
                .iter()
                .any(|token| lower.contains(token))
            || (lower.contains("echo ") && lower.contains('>'))
        {
            return BashCommandClass::MutationRisk;
        }

        if self
            .tools
            .research_safe_bash_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix) || lower == prefix.trim_end_matches(' '))
        {
            return BashCommandClass::ResearchSafe;
        }

        BashCommandClass::MutationRisk
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

    pub fn build_truncation_notice(&self, dropped_count: usize) -> String {
        format!(
            "[Previous {dropped_count} messages truncated due to context length. \
Preserved via fresh system prompt each turn: {}. Use tools to re-read files if you need earlier context.]",
            self.compaction.preserved_sections.join(", ")
        )
    }

    pub fn render_system_prompt(&self, ctx: &BehaviorPromptContext<'_>) -> String {
        let mut prompt = String::from(
            "You are TopAgent, a coding assistant operating within a workspace directory. \
All file paths are relative to this workspace root.\n\n",
        );

        self.render_identity_section(&mut prompt);
        self.render_task_section(&mut prompt);
        self.render_planning_section(&mut prompt);
        self.render_tool_section(&mut prompt);
        self.render_mutation_section(&mut prompt);
        self.render_approval_section(&mut prompt);
        self.render_output_section(&mut prompt);
        self.render_memory_section(&mut prompt);
        self.render_generated_tool_section(&mut prompt);
        self.render_compaction_section(&mut prompt);
        self.render_available_tools_section(&mut prompt, ctx.available_tools, ctx.external_tools);

        match ctx.project_instructions {
            Some(project_instructions) => {
                prompt.push_str("## Project Instructions (from TOPAGENT.md)\n\n");
                prompt.push_str(project_instructions);
                prompt.push('\n');
            }
            None => prompt.push_str(NO_PROJECT_INSTRUCTIONS_NOTE),
        }

        if let Some(memory_context) = ctx.memory_context {
            prompt.push_str("\n## Workspace Memory\n\n");
            prompt.push_str(memory_context);
            prompt.push('\n');
        }

        if !ctx.generated_tool_warnings.is_empty() {
            prompt.push_str("\n## Generated Tool Warnings\n\n");
            prompt.push_str(
                "Some generated tools in `.topagent/tools/` are currently unavailable. Treat this as current workspace state.\n",
            );
            for warning in ctx.generated_tool_warnings {
                prompt.push_str(&format!("- {warning}\n"));
            }
            prompt.push_str(
                "Do not assume unavailable generated tools can be called unless they appear in the available tools list.\n",
            );
        }

        if let Some(plan) = ctx.current_plan {
            if !plan.is_empty() {
                prompt.push_str("\n## Current Plan\n\n");
                prompt.push_str(&plan.format_for_display());
            }
        }

        if ctx.planning_required_now {
            prompt.push_str("\n## Planning Required\n\n");
            prompt.push_str("This task is currently plan-required.\n");
            for (index, stage) in self.planning.workflow.iter().enumerate() {
                prompt.push_str(&format!("{}. {stage}\n", index + 1));
            }
            prompt.push_str(
                "\nUse update_plan before mutation-risk tools, then execute at least one concrete plan step before verification-only tools.\n",
            );
        }

        prompt
    }

    fn render_identity_section(&self, prompt: &mut String) {
        prompt.push_str("## Product Identity\n\n");
        prompt.push_str(&format!(
            "- Primary channels: {}\n",
            self.identity.primary_channels.join(", ")
        ));
        prompt.push_str(&format!(
            "- Execution model: {}\n",
            self.identity.execution_model
        ));
        prompt.push_str(&format!("- Scope: {}\n", self.identity.scope));
        prompt.push_str(&format!(
            "- Control model: {}\n",
            self.identity.operator_model
        ));
        prompt.push_str(&format!(
            "- Provider default: {}\n",
            self.identity.provider_default
        ));
        prompt.push_str(&format!(
            "- Strengths: {}\n",
            self.identity.strengths.join(", ")
        ));
        prompt.push_str(&format!(
            "- Non-goals: {}\n\n",
            self.identity.non_goals.join(", ")
        ));
    }

    fn render_task_section(&self, prompt: &mut String) {
        prompt.push_str("## Task Classification\n\n");
        prompt.push_str(
            "- Use upfront planning for broad, multi-step, or explicitly plan-oriented work.\n",
        );
        prompt.push_str(&format!(
            "- Short narrow instructions at or under {} characters can execute directly.\n",
            self.task.direct_instruction_length_threshold
        ));
        prompt.push_str("- Task modes: execute, inspect, verify.\n");
        prompt.push_str("- Inspect and verify tasks finish without making code changes.\n\n");
    }

    fn render_planning_section(&self, prompt: &mut String) {
        prompt.push_str("## Plan-Before-Act Policy\n\n");
        prompt.push_str(&format!(
            "- Default planning gate enabled: {}\n",
            self.planning.require_plan_by_default
        ));
        prompt.push_str(&format!(
            "- Workflow: {}\n",
            self.planning.workflow.join(" -> ")
        ));
        prompt.push_str("- Planning tool: update_plan.\n");
        prompt.push_str(
            "- Research-safe reads and repo inspection can happen before a plan; mutation-risk actions cannot.\n",
        );
        prompt.push_str(&format!(
            "- Auto-plan fallback after {} blocked mutations, {} planning-phase turns, or {} text bail-outs.\n",
            self.planning.max_blocked_mutations_before_auto_plan,
            self.planning.max_research_steps_without_plan,
            self.planning.max_text_redirects_before_auto_plan
        ));
        prompt.push_str(&format!(
            "- Runtime escalates narrow direct work into plan-required after {} distinct changed files without a plan.\n\n",
            self.planning.unplanned_mutation_escalation_threshold
        ));
    }

    fn render_tool_section(&self, prompt: &mut String) {
        prompt.push_str("## Tool Routing Rules\n\n");
        prompt.push_str(&format!(
            "- Repo-awareness tools: {}\n",
            self.tools.repo_awareness_tools.join(", ")
        ));
        prompt.push_str(&format!(
            "- Planning tools: {}\n",
            self.tools.planning_tools.join(", ")
        ));
        prompt.push_str(&format!(
            "- Durable memory write tools: {}\n",
            self.tools.memory_write_tools.join(", ")
        ));
        if self.generated_tools.authoring_enabled {
            prompt.push_str(&format!(
                "- Generated-tool authoring tools enabled: {}\n",
                self.tools.generated_tool_authoring_tools.join(", ")
            ));
        } else {
            prompt.push_str("- Generated-tool authoring tools are disabled for this run.\n");
        }
        prompt.push_str(
            "- External tools declare effect as read_only, verification_only, or execution_started.\n",
        );
        prompt.push_str(
            "- Bash commands are routed as research-safe, verification, or mutation-risk.\n\n",
        );
    }

    fn render_mutation_section(&self, prompt: &mut String) {
        prompt.push_str("## Mutation And Destructive-Action Rules\n\n");
        prompt.push_str(&format!(
            "- Structured mutation tools: {}\n",
            self.mutation.mutation_tools.join(", ")
        ));
        prompt.push_str("- Prefer read before edit when exact content matters.\n");
        prompt.push_str("- Treat redirected shell writes, pipes, and filesystem-changing shell commands as mutation-risk.\n");
        prompt.push_str(&format!(
            "- Generated-tool surface mutations: {}\n",
            self.mutation.generated_tool_surface_tools.join(", ")
        ));
        prompt.push_str("- Never use tools to reveal or relay credentials.\n\n");
    }

    fn render_approval_section(&self, prompt: &mut String) {
        prompt.push_str("## Approval Triggers\n\n");
        if self.approval.mailbox_available {
            prompt.push_str("- Approval mailbox is available.\n");
        } else {
            prompt.push_str(
                "- Approval mailbox is not built yet. Treat the following triggers as advisory ask-first rules in chat.\n",
            );
        }
        for rule in self.approval.triggers {
            prompt.push_str(&format!("- {}: {}\n", rule.label, rule.rationale));
        }
        prompt.push('\n');
    }

    fn render_output_section(&self, prompt: &mut String) {
        prompt.push_str("## Output Contract\n\n");
        prompt.push_str("- After tool use, provide a concise final answer instead of replaying raw tool output.\n");
        prompt.push_str("- When files change or verification runs, include proof-of-work with files changed, change summary, verification, unresolved issues, and workspace warnings when present.\n");
        prompt.push_str(
            "- When asked to verify, show diff, or confirm changes, provide actual evidence.\n",
        );
        prompt.push_str("- Never reveal secrets, tokens, passwords, or credential values.\n\n");
    }

    fn render_memory_section(&self, prompt: &mut String) {
        prompt.push_str("## Memory Write Rules\n\n");
        prompt.push_str("- Loaded workspace memory is advisory; re-verify against the current repo and runtime state.\n");
        prompt.push_str("- Current workspace state wins over memory when they conflict.\n");
        prompt.push_str(&format!(
            "- Durable memory writes are limited to: {}\n",
            self.memory.durable_write_tools.join(", ")
        ));
        prompt.push_str(&format!(
            "- Never store: {}\n\n",
            self.memory.never_store.join(", ")
        ));
    }

    fn render_generated_tool_section(&self, prompt: &mut String) {
        prompt.push_str("## Generated-Tool Policy\n\n");
        prompt.push_str(&format!(
            "- Authoring enabled for this run: {}\n",
            self.generated_tools.authoring_enabled
        ));
        prompt.push_str("- Generated tools are workspace-local and disposable.\n");
        prompt.push_str("- Only verified generated tools are callable.\n");
        prompt.push_str(
            "- Create and repair flows must verify before relying on a generated tool.\n",
        );
        prompt.push_str("- Surface unavailable generated tools as warnings instead of assuming they can be called.\n\n");
    }

    fn render_compaction_section(&self, prompt: &mut String) {
        prompt.push_str("## Compaction Preservation\n\n");
        prompt.push_str(&format!(
            "- History truncates after {} non-system messages.\n",
            self.compaction.max_messages_before_truncation
        ));
        prompt.push_str(&format!(
            "- Keep roughly the most recent 1/{} of the history when truncating.\n",
            self.compaction.keep_recent_divisor
        ));
        prompt.push_str(&format!(
            "- Refresh system prompt each turn: {}\n",
            self.compaction.refresh_system_prompt_each_turn
        ));
        prompt.push_str(&format!(
            "- Preserve via system prompt refresh: {}\n\n",
            self.compaction.preserved_sections.join(", ")
        ));
    }

    fn render_available_tools_section(
        &self,
        prompt: &mut String,
        available_tools: &[ToolSpec],
        external_tools: &[ToolSpec],
    ) {
        if !external_tools.is_empty() {
            prompt.push_str("## External Tools\n\n");
            for tool in external_tools {
                prompt.push_str(&format!("- {}: {}\n", tool.name, tool.description));
            }
            prompt.push('\n');
        }

        prompt.push_str("## Available Tools\n\n");
        for tool in available_tools {
            prompt.push_str(&format!("- {}: {}\n", tool.name, tool.description));
        }
        prompt.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_render_system_prompt_includes_contract_sections() {
        let contract = BehaviorContract::default();
        let plan = {
            let mut plan = Plan::new();
            plan.add_item("Inspect src/lib.rs".to_string());
            plan
        };
        let prompt = contract.render_system_prompt(&BehaviorPromptContext {
            available_tools: &[ToolSpec::read()],
            external_tools: &[],
            project_instructions: Some("# Repo rules"),
            memory_context: Some("Treat memory as hints, not truth."),
            current_plan: Some(&plan),
            generated_tool_warnings: &["broken_tool: missing script.sh".to_string()],
            planning_required_now: true,
        });

        assert!(prompt.contains("## Product Identity"));
        assert!(prompt.contains("## Output Contract"));
        assert!(prompt.contains("## Memory Write Rules"));
        assert!(prompt.contains("## Generated-Tool Policy"));
        assert!(prompt.contains("## Compaction Preservation"));
        assert!(prompt.contains("Current plan"));
        assert!(prompt.contains("broken_tool: missing script.sh"));
    }
}

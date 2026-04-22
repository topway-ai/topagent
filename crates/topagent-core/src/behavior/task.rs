use super::{BashCommandClass, BehaviorContract, PlanningPolicy, PreExecutionState, TaskPolicy};
use crate::plan::TaskMode;
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

pub(super) fn default_task_policy() -> TaskPolicy {
    TaskPolicy {
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
            "what is", "where is", "how do", "how does", "show me", "list ", "find ", "search ",
            "get ", "read ",
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
    }
}

pub(super) fn default_planning_policy(options: &RuntimeOptions) -> PlanningPolicy {
    PlanningPolicy {
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
    }
}

impl TaskPolicy {
    pub(crate) fn classify_task_fast_path(&self, instruction: &str) -> Option<bool> {
        let lower = instruction.to_lowercase();

        if self
            .explicit_plan_phrases
            .iter()
            .any(|phrase| lower.contains(phrase))
        {
            return Some(true);
        }

        if self
            .broad_scope_phrases
            .iter()
            .any(|phrase| lower.contains(phrase))
        {
            return Some(true);
        }

        if self
            .trivial_query_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            && lower.len() < self.direct_instruction_length_threshold
        {
            return Some(false);
        }

        if lower.len() <= self.direct_instruction_length_threshold {
            return Some(false);
        }

        None
    }

    pub(crate) fn build_task_classification_messages(&self, instruction: &str) -> (String, String) {
        (
            self.classification_system_prompt.to_string(),
            instruction.to_string(),
        )
    }

    pub(crate) fn task_mode_fast_path(&self, instruction: &str) -> Option<TaskMode> {
        let lower = instruction.to_lowercase();
        self.mutation_intent_cues
            .iter()
            .any(|cue| lower.contains(cue))
            .then_some(TaskMode::PlanAndExecute)
    }

    pub(crate) fn build_task_mode_messages(&self, instruction: &str) -> (String, String) {
        (
            self.task_mode_system_prompt.to_string(),
            instruction.to_string(),
        )
    }
}

impl PlanningPolicy {
    pub(crate) fn build_plan_generation_prompt(&self, instruction: &str) -> (String, String) {
        (
            self.plan_generation_system_prompt.to_string(),
            format!("Create a plan for this task:\n\n{instruction}"),
        )
    }
}

impl BehaviorContract {
    pub fn classify_task_fast_path(&self, instruction: &str) -> Option<bool> {
        self.task.classify_task_fast_path(instruction)
    }

    pub fn build_task_classification_messages(&self, instruction: &str) -> (String, String) {
        self.task.build_task_classification_messages(instruction)
    }

    pub fn task_mode_fast_path(&self, instruction: &str) -> Option<TaskMode> {
        self.task.task_mode_fast_path(instruction)
    }

    pub fn build_task_mode_messages(&self, instruction: &str) -> (String, String) {
        self.task.build_task_mode_messages(instruction)
    }

    pub fn build_plan_generation_prompt(&self, instruction: &str) -> (String, String) {
        self.planning.build_plan_generation_prompt(instruction)
    }

    pub fn planning_block_message(
        &self,
        tool_name: &str,
        bash_command: Option<&str>,
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
}

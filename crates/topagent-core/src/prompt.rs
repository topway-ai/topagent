use crate::behavior::{BehaviorContract, RunStateSnapshot};
use crate::plan::Plan;
use crate::tool_spec::ToolSpec;

pub const NO_PROJECT_INSTRUCTIONS_NOTE: &str =
    "\n[Note: No TOPAGENT.md file is present in the workspace root. Do not try to read it.]\n";
pub const NO_PI_MD_NOTE: &str = NO_PROJECT_INSTRUCTIONS_NOTE;

pub struct BehaviorPromptContext<'a> {
    pub available_tools: &'a [ToolSpec],
    pub external_tools: &'a [ToolSpec],
    pub project_instructions: Option<&'a str>,
    pub operator_context: Option<&'a str>,
    pub memory_context: Option<&'a str>,
    pub current_plan: Option<&'a Plan>,
    pub run_state: Option<&'a RunStateSnapshot>,
    pub generated_tool_warnings: &'a [String],
    pub hook_summary_lines: &'a [String],
    pub planning_required_now: bool,
    pub approval_mailbox_available: bool,
}

pub fn build_system_prompt(tools: &[ToolSpec], external_tools: &[ToolSpec]) -> String {
    BehaviorContract::default().render_system_prompt(&BehaviorPromptContext {
        available_tools: tools,
        external_tools,
        project_instructions: None,
        operator_context: None,
        memory_context: None,
        current_plan: None,
        run_state: None,
        generated_tool_warnings: &[],
        hook_summary_lines: &[],
        planning_required_now: false,
        approval_mailbox_available: false,
    })
}

impl BehaviorContract {
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
        self.render_approval_section(&mut prompt, ctx);
        self.render_output_section(&mut prompt);
        self.render_memory_section(&mut prompt);
        self.render_generated_tool_section(&mut prompt);
        self.render_compaction_section(&mut prompt);
        self.render_available_tools_section(&mut prompt, ctx.available_tools, ctx.external_tools);

        if !ctx.hook_summary_lines.is_empty() {
            prompt.push_str("## Workspace Lifecycle Hooks\n\n");
            prompt.push_str("The following workspace-local hooks are active for this run. They intercept lifecycle boundaries deterministically.\n");
            prompt.push_str("Hooks cannot bypass approval gates, trust boundaries, or durable-memory promotion policy.\n");
            for line in ctx.hook_summary_lines {
                prompt.push_str(&format!("- {line}\n"));
            }
            prompt.push('\n');
        }

        match ctx.project_instructions {
            Some(project_instructions) => {
                prompt.push_str("## Project Instructions (from TOPAGENT.md)\n\n");
                prompt.push_str(project_instructions);
                prompt.push('\n');
            }
            None => prompt.push_str(NO_PROJECT_INSTRUCTIONS_NOTE),
        }

        if let Some(operator_context) = ctx.operator_context {
            prompt.push_str("\n## Operator Model\n\n");
            prompt.push_str(operator_context);
            prompt.push('\n');
        }

        if let Some(memory_context) = ctx.memory_context {
            prompt.push_str("\n## Workspace Memory\n\n");
            prompt.push_str(memory_context);
            prompt.push('\n');
        }

        if let Some(run_state) = ctx.run_state {
            self.render_run_state_section(&mut prompt, run_state);
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

    fn render_run_state_section(&self, prompt: &mut String, run_state: &RunStateSnapshot) {
        prompt.push_str("\n## Active Run State\n\n");

        if let Some(objective) = &run_state.objective {
            prompt.push_str(&format!("- Current objective: {objective}\n"));
        }

        if run_state.memory_context_loaded {
            prompt.push_str("- Workspace memory briefing is loaded in this prompt.\n");
        }

        if run_state.blockers.is_empty() {
            prompt.push_str("- Current blockers: none.\n");
        } else {
            prompt.push_str("- Current blockers:\n");
            for blocker in &run_state.blockers {
                prompt.push_str(&format!("  - {blocker}\n"));
            }
        }

        if !run_state.pending_approvals.is_empty() {
            prompt.push_str("- Pending approvals:\n");
            for approval in &run_state.pending_approvals {
                prompt.push_str(&format!("  - {approval}\n"));
            }
        }

        if !run_state.recent_approval_decisions.is_empty() {
            prompt.push_str("- Recent approval decisions still relevant to this run:\n");
            for decision in &run_state.recent_approval_decisions {
                prompt.push_str(&format!("  - {decision}\n"));
            }
        }

        if !run_state.active_files.is_empty() {
            prompt.push_str(&format!(
                "- Active files / working set: {}\n",
                run_state.active_files.join(", ")
            ));
        }

        if !run_state.proof_of_work_anchors.is_empty() {
            prompt.push_str("- Proof-of-work anchors:\n");
            for anchor in &run_state.proof_of_work_anchors {
                prompt.push_str(&format!("  - {anchor}\n"));
            }
        }

        if !run_state.trust_notes.is_empty() {
            prompt.push_str("- Trust notes:\n");
            for note in &run_state.trust_notes {
                prompt.push_str(&format!("  - {note}\n"));
            }
        }

        if !run_state.hook_notes.is_empty() {
            prompt.push_str("- Workspace hook notes:\n");
            for note in &run_state.hook_notes {
                prompt.push_str(&format!("  - {note}\n"));
            }
        }
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
        prompt.push_str(
            "- For repository file or LOC counts, prefer tracked-file commands such as `git ls-files` or `rg --files` over ad hoc extension lists.\n\n",
        );
        prompt.push_str(
            "- Bash commands run in a sandbox with no outbound network; do not use curl, wget, or similar fetch commands.\n\n",
        );
    }

    fn render_mutation_section(&self, prompt: &mut String) {
        prompt.push_str("## Mutation And Destructive-Action Rules\n\n");
        prompt.push_str(&format!(
            "- Structured mutation tools: {}\n",
            self.mutation.mutation_tools.join(", ")
        ));
        prompt.push_str("- Prefer read before edit when exact content matters.\n");
        prompt.push_str(
            "- Treat file-writing redirections, explicit filesystem-changing shell commands, and unknown shell commands as mutation-risk; read-only pipelines remain research-safe or verification.\n",
        );
        prompt.push_str(&format!(
            "- Generated-tool surface mutations: {}\n",
            self.mutation.generated_tool_surface_tools.join(", ")
        ));
        prompt.push_str("- Never use tools to reveal or relay credentials.\n\n");
    }

    fn render_approval_section(&self, prompt: &mut String, ctx: &BehaviorPromptContext<'_>) {
        prompt.push_str("## Approval Triggers\n\n");
        if ctx.approval_mailbox_available || self.approval.mailbox_available {
            prompt.push_str("- Approval mailbox is available.\n");
        } else {
            prompt.push_str(
                "- Approval mailbox is unavailable for this run. If a trigger fires, stop and report that approval is required.\n",
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
        if self.memory.keep_index_tiny {
            prompt.push_str("- Keep durable memory indexes tiny and pointer-oriented.\n");
        }
        if self.memory.index_is_pointer_only {
            prompt.push_str("- Store richer durable notes in topic files instead of the index.\n");
        }
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
        prompt.push_str(
            "- Surface only bounded runtime unavailability warnings by default; deeper health checks belong to explicit maintenance flows.\n",
        );
        prompt.push_str("- Revalidate a generated tool on use instead of assuming it stayed healthy after startup.\n\n");
    }

    fn render_compaction_section(&self, prompt: &mut String) {
        prompt.push_str("## Compaction Preservation\n\n");
        prompt.push_str(&format!(
            "- Micro-compaction starts after {} non-system messages.\n",
            self.compaction.micro_trigger_messages
        ));
        prompt.push_str(&format!(
            "- Auto-compaction starts after {} non-system messages.\n",
            self.compaction.max_messages_before_truncation
        ));
        prompt.push_str(&format!(
            "- Keep roughly the most recent 1/{} of the history when truncating.\n",
            self.compaction.keep_recent_divisor
        ));
        prompt.push_str(&format!(
            "- Full rebuild keeps roughly the last {} messages and rebuilds the rest from canonical runtime artifacts.\n",
            self.full_rebuild_recent_message_count()
        ));
        prompt.push_str(&format!(
            "- Compact at most {} older tool traces into the transcript summary.\n",
            self.compaction.max_compacted_trace_lines
        ));
        prompt.push_str(&format!(
            "- Preserve up to {} recent approval decisions and {} proof-of-work anchors explicitly.\n",
            self.compaction.max_recent_approval_decisions,
            self.compaction.max_recent_proof_of_work_anchors
        ));
        prompt.push_str(&format!(
            "- Disable auto-compaction after {} consecutive failures and fall back to blunt truncation.\n",
            self.compaction.max_failed_auto_compactions
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
            operator_context: Some(
                "[response_style] concise final answers :: Keep final responses concise.",
            ),
            memory_context: Some("Treat memory as hints, not truth."),
            current_plan: Some(&plan),
            run_state: Some(&RunStateSnapshot {
                objective: Some("Fix the parser and keep tests passing".to_string()),
                blockers: vec!["Approval denied for git commit".to_string()],
                pending_approvals: vec!["apr-3 [pending] git commit: release".to_string()],
                recent_approval_decisions: vec!["apr-2 [denied] delete generated tool".to_string()],
                active_files: vec!["src/lib.rs".to_string()],
                proof_of_work_anchors: vec!["verification: cargo test --lib (exit 0)".to_string()],
                trust_notes: vec![
                    "Low-trust content is active in this run: prior transcript.".to_string()
                ],
                hook_notes: vec!["[fmt] run cargo fmt after editing Rust files".to_string()],
                memory_context_loaded: true,
            }),
            generated_tool_warnings: &["broken_tool: missing script.sh".to_string()],
            hook_summary_lines: &["pre_tool: bash guard [filter: bash]".to_string()],
            planning_required_now: true,
            approval_mailbox_available: true,
        });

        assert!(prompt.contains("## Product Identity"));
        assert!(prompt.contains("## Operator Model"));
        assert!(prompt.contains("## Active Run State"));
        assert!(prompt.contains("## Output Contract"));
        assert!(prompt.contains("## Memory Write Rules"));
        assert!(prompt.contains("## Generated-Tool Policy"));
        assert!(prompt.contains("## Compaction Preservation"));
        assert!(prompt.contains("Fix the parser and keep tests passing"));
        assert!(prompt.contains("apr-3 [pending] git commit: release"));
        assert!(prompt.contains("Current plan"));
        assert!(prompt.contains("broken_tool: missing script.sh"));
        assert!(prompt.contains("git ls-files"));
        assert!(prompt.contains("Trust notes"));
        assert!(prompt.contains("Workspace hook notes"));
        assert!(prompt.contains("cargo fmt"));
        assert!(prompt.contains("Workspace Lifecycle Hooks"));
        assert!(prompt.contains("bash guard"));
    }
}

use super::{Agent, ExecutionStage};
use crate::context::ExecutionContext;
use crate::project::get_project_instructions_or_error;
use crate::prompt::BehaviorPromptContext;
use crate::prompt_budget::PromptBudgetUsage;
use crate::Result;

struct RunPromptContextBuilder<'a> {
    agent: &'a Agent,
    ctx: &'a ExecutionContext,
}

impl<'a> RunPromptContextBuilder<'a> {
    fn new(agent: &'a Agent, ctx: &'a ExecutionContext) -> Self {
        Self { agent, ctx }
    }

    fn build(self) -> Result<String> {
        let project_instructions = get_project_instructions_or_error(&self.ctx.workspace_root)?;
        let available_tools = self
            .agent
            .harness
            .available_skills(self.ctx, self.agent.current_agent_phase());
        let plan_guard = self.agent.plan.lock().ok();
        let current_plan = plan_guard
            .as_ref()
            .filter(|plan| !plan.is_empty())
            .map(|plan| &**plan);
        let plan_exists = current_plan.is_some();
        let run_state = self.agent.run_state.build_snapshot(
            &self.agent.behavior,
            self.ctx,
            self.agent.planning.is_active() && !plan_exists,
        );
        let current_phase = if self.agent.planning.is_active() && !plan_exists {
            crate::harness::AgentPhase::Plan
        } else if self.agent.planning.is_active() && plan_exists {
            crate::harness::AgentPhase::Patch
        } else {
            match self.agent.execution_stage {
                ExecutionStage::Research => crate::harness::AgentPhase::Investigate,
                ExecutionStage::Edit => crate::harness::AgentPhase::Patch,
                ExecutionStage::Review => crate::harness::AgentPhase::Verify,
            }
        };
        let queue = current_plan.map(|plan| plan.queue_status());
        let run_checkpoint = self.agent.run_state.build_checkpoint(
            &self.agent.behavior,
            self.ctx,
            queue,
            self.agent.task_mode(),
            current_phase,
            self.agent.planning.is_active() && !plan_exists,
        );

        let prompt = self
            .agent
            .behavior
            .render_system_prompt(&BehaviorPromptContext {
                available_tools: &available_tools,
                project_instructions: project_instructions.as_deref(),
                operator_context: self.ctx.operator_context(),
                memory_context: self.ctx.memory_context(),
                current_plan,
                run_state: Some(&run_state),
                run_checkpoint: Some(&run_checkpoint),
                planning_required_now: self.agent.planning.is_active() && !plan_exists,
                approval_mailbox_available: self.ctx.approval_mailbox().is_some(),
            });
        let checkpoint_summary = run_checkpoint.render_compact();
        let usage = PromptBudgetUsage::from_prompt_parts(
            &prompt,
            self.ctx.memory_context(),
            Some(&checkpoint_summary),
            &available_tools,
        );
        if let Err(message) = usage.validate_default_mode() {
            tracing::warn!(%message, "prompt budget exceeded");
        }

        Ok(prompt)
    }
}

impl Agent {
    pub(super) fn build_run_system_prompt(&self, ctx: &ExecutionContext) -> Result<String> {
        RunPromptContextBuilder::new(self, ctx).build()
    }
}

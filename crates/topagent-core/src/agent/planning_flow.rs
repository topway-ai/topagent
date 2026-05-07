use super::{Agent, ExecutionStage};
use crate::compaction::CompactionRuntimeState;
use crate::context::ExecutionContext;
use crate::plan;
use crate::progress::ProgressUpdate;
use crate::{Error, Message, ProviderResponse, Result};

impl Agent {
    pub(super) fn plan_exists(&self) -> bool {
        self.plan
            .lock()
            .map(|plan| !plan.is_empty())
            .unwrap_or(false)
    }

    pub(super) fn deactivate_planning_gate(&mut self) {
        self.planning.deactivate();
    }

    /// Pop a previous redirect message (if present) and replace it with the
    /// model's response followed by a fresh redirect nudge.
    pub(super) fn redirect_to_planning(&mut self, msg: Message, redirect_msg: &str) {
        self.session
            .pop_last_if(|m| m.as_text().map(|t| t == redirect_msg).unwrap_or(false));
        self.session.add_message(msg);
        self.session.add_message(Message::user(redirect_msg));
    }

    /// Classify whether the task requires upfront planning.
    ///
    /// Uses a two-tier system:
    /// 1. Heuristic fast path for clear-cut cases (instant, no API call).
    /// 2. Lightweight LLM classification call for ambiguous cases.
    ///
    /// Falls back to `false` (direct execution) if the LLM call fails.
    fn classify_task(
        &self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<bool> {
        match self.behavior.classify_task_fast_path(instruction) {
            Some(result) => Ok(result),
            None => self.classify_task_with_llm(instruction, cancel),
        }
    }

    fn classify_task_with_llm(
        &self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<bool> {
        let (system_prompt, user_msg) = self
            .behavior
            .build_task_classification_messages(instruction);
        let messages = vec![Message::system(system_prompt), Message::user(user_msg)];
        let route = self.resolved_route.clone();

        match self
            .provider
            .complete_with_cancel(&messages, &route, cancel)
        {
            Ok(ProviderResponse::Message(msg)) => Ok(msg
                .as_text()
                .map(plan::parse_classification_response)
                .unwrap_or(false)),
            Ok(_) => Ok(false),
            Err(Error::Stopped(_)) => Err(Self::stop_error()),
            Err(_) => Ok(false),
        }
    }

    fn classify_task_mode(
        &self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<plan::TaskMode> {
        match self.behavior.task_mode_fast_path(instruction) {
            Some(mode) => Ok(mode),
            None => self.classify_task_mode_with_llm(instruction, cancel),
        }
    }

    fn classify_task_mode_with_llm(
        &self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<plan::TaskMode> {
        let (system_prompt, user_msg) = self.behavior.build_task_mode_messages(instruction);
        let messages = vec![Message::system(system_prompt), Message::user(user_msg)];
        let route = self.resolved_route.clone();

        match self
            .provider
            .complete_with_cancel(&messages, &route, cancel)
        {
            Ok(ProviderResponse::Message(msg)) => Ok(msg
                .as_text()
                .and_then(plan::parse_task_mode_response)
                .unwrap_or(plan::TaskMode::PlanAndExecute)),
            Ok(_) => Ok(plan::TaskMode::PlanAndExecute),
            Err(Error::Stopped(_)) => Err(Self::stop_error()),
            Err(_) => Ok(plan::TaskMode::PlanAndExecute),
        }
    }

    /// Break a planning deadlock by generating a real plan via the LLM.
    /// Falls back to a minimal emergency plan if the LLM call fails.
    /// Always deactivates the planning gate afterward.
    pub(super) fn generate_or_fallback_plan(
        &mut self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<()> {
        if self.plan_exists() {
            self.deactivate_planning_gate();
            return Ok(());
        }

        // Try a dedicated LLM plan-generation call.
        if self.try_generate_plan(instruction, cancel)? {
            self.deactivate_planning_gate();
            return Ok(());
        }

        // LLM failed -- create a minimal emergency plan so the agent can proceed.
        if let Ok(mut plan) = self.plan.lock() {
            plan.clear();
            plan.add_item("Execute the requested changes".to_string());
            plan.add_item("Verify the result".to_string());
        }
        self.deactivate_planning_gate();
        Ok(())
    }

    /// Attempt to generate a concrete plan via a single LLM call.
    /// Returns true if a non-empty plan was created.
    fn try_generate_plan(
        &mut self,
        instruction: &str,
        cancel: Option<&crate::CancellationToken>,
    ) -> Result<bool> {
        let prompt = self.behavior.build_plan_generation_prompt(instruction);
        let messages = vec![Message::system(prompt.0), Message::user(prompt.1)];
        let route = self.resolved_route.clone();

        let text = match self
            .provider
            .complete_with_cancel(&messages, &route, cancel)
        {
            Ok(ProviderResponse::Message(msg)) => msg.as_text().map(|s| s.to_string()),
            Ok(_) => None,
            Err(Error::Stopped(_)) => return Err(Self::stop_error()),
            Err(_) => None,
        };

        let Some(text) = text else { return Ok(false) };
        let items = plan::parse_plan_generation_response(&text);
        if items.is_empty() {
            return Ok(false);
        }

        if let Ok(mut plan) = self.plan.lock() {
            plan.clear();
            for item in items {
                plan.add_item(item);
            }
        }
        Ok(true)
    }

    pub(super) fn note_planning_block(
        &mut self,
        ctx: &ExecutionContext,
        instruction: &str,
    ) -> Result<()> {
        if !self.planning.is_active() || self.plan_exists() {
            self.planning.reset_block_count();
            return Ok(());
        }

        self.planning.note_block();
        if self.planning.block_count()
            >= self
                .behavior
                .planning
                .max_blocked_mutations_before_auto_plan
        {
            self.generate_or_fallback_plan(instruction, ctx.cancel_token())?;
        }

        Ok(())
    }

    /// Check whether a task that was *not* initially classified as
    /// plan-required should be escalated based on runtime mutation signals.
    /// Activates the planning gate if multiple distinct files have been
    /// changed without any plan in place.
    pub(super) fn maybe_escalate_to_planning(&mut self) {
        let distinct_files = self.run_state.changed_file_count();
        if self.behavior.should_escalate_to_planning(
            self.planning.is_active(),
            self.planning.is_escalated(),
            self.plan_exists(),
            distinct_files,
        ) {
            self.planning.escalate();
            self.emit_progress(ProgressUpdate::planning());
        }
    }

    pub(super) fn reset_run_state(
        &mut self,
        ctx: &ExecutionContext,
        instruction: &str,
    ) -> Result<()> {
        let workspace_root = &ctx.workspace_root;
        self.run_state.reset(workspace_root, instruction);
        self.compaction_state = CompactionRuntimeState::default();
        self.last_task_result = None;
        self.durable_memory_written_this_run = false;
        self.eval_model_turns = 0;
        self.eval_skill_calls = 0;
        self.eval_approval_blocks = 0;

        let required_for_task = self.behavior.planning.require_plan_by_default
            && self.classify_task(instruction, ctx.cancel_token())?;
        let task_mode = if required_for_task {
            self.classify_task_mode(instruction, ctx.cancel_token())?
        } else {
            plan::TaskMode::PlanAndExecute
        };
        self.planning.activate(required_for_task, task_mode);
        self.execution_stage = ExecutionStage::Research;
        Ok(())
    }
}

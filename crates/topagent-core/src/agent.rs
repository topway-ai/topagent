use crate::approval::ApprovalCheck;
use crate::behavior::{
    BashCommandClass, BehaviorContract, BehaviorPromptContext, PreExecutionState, RunStateSnapshot,
};
use crate::command_exec::CommandSandboxPolicy;
use crate::compaction::{CompactionRuntimeState, TranscriptCompactor};
use crate::context::{ExecutionContext, ToolContext};
use crate::external::{ExternalToolEffect, ExternalToolRegistry};
use crate::hooks::HookRegistry;
use crate::model::ModelRoute;
use crate::plan::{self, Plan};
use crate::progress::{ProgressCallback, ProgressUpdate};
use crate::project::get_project_instructions_or_error;
use crate::runtime::RuntimeOptions;
use crate::session::Session;
use crate::task_result::{TaskEvidence, TaskResult, VerificationCommand};
use crate::tool_genesis::{
    CreateToolTool, DeleteGeneratedToolTool, ListGeneratedToolsTool, RepairToolTool, ToolGenesis,
};
use crate::tools::{SaveLessonTool, SavePlanTool, Tool, ToolRegistry, UpdatePlanTool};
use crate::{Error, Message, Provider, ProviderResponse, Result, ToolSpec};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

// ── Planning deadlock thresholds ──
//
// Two independent counters protect against distinct planning failures.
// They are intentionally separate:
//
// 1. `planning_block_count` (vs behavior.planning.max_blocked_mutations_before_auto_plan):
//    Counts consecutive mutation-tool calls blocked by the planning gate.
//    Covers: model actively tries to mutate without creating a plan.
//
// 2. `planning_phase_steps` (vs behavior.planning.max_research_steps_without_plan):
//    Counts total loop iterations while gate is active and plan is empty.
//    Covers: model loops in research tools without ever attempting mutation
//    or planning.
//
// Both trigger the same fallback: try a dedicated LLM plan-generation call,
// and if that fails, create a minimal emergency plan.
//
// `planning_redirects` (vs behavior.planning.max_text_redirects_before_auto_plan):
//    Counts text-response bail-outs during planning phase.
//    Covers: model tries to return a final answer without planning.

const WORKSPACE_EXTERNAL_TOOLS_PATH: &str = ".topagent/external-tools.json";
pub struct Agent {
    session: Session,
    provider: Box<dyn Provider>,
    tools: ToolRegistry,
    external_tools: ExternalToolRegistry,
    options: RuntimeOptions,
    behavior: BehaviorContract,
    plan: Arc<Mutex<Plan>>,
    hooks: HookRegistry,
    current_objective: Option<String>,
    changed_files: RefCell<Vec<String>>,
    active_files: RefCell<Vec<String>>,
    bash_history: RefCell<Vec<(String, String, i32)>>,
    planning_gate_active: bool,
    planning_required_for_task: bool,
    task_mode: plan::TaskMode,
    /// Set to true if the planning gate was activated mid-run by runtime
    /// escalation (risk #3). Prevents re-escalation after auto-plan.
    planning_escalated: bool,
    resolved_route: ModelRoute,
    execution_stage: ExecutionStage,
    external_tool_ran: RefCell<bool>,
    run_baseline: RefCell<Option<RunBaseline>>,
    progress_callback: Option<ProgressCallback>,
    planning_block_count: usize,
    compaction_state: CompactionRuntimeState,
    generated_tool_warnings: Vec<String>,
}

struct RunBaseline {
    pre_existing_dirty: Vec<String>,
    pre_existing_hashes: HashMap<String, String>,
    pre_existing_unattributed: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionStage {
    #[default]
    Research,
    Edit,
    Review,
}

/// Result of a preflight check that blocked tool execution.
struct PreflightBlock {
    message: String,
    is_planning_block: bool,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        Self::with_options(provider, tools, RuntimeOptions::default())
    }

    pub fn with_route(
        provider: Box<dyn Provider>,
        route: ModelRoute,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        Self::with_route_and_options(provider, route, tools, options)
    }

    pub fn with_options(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        Self::with_route_and_options(provider, ModelRoute::default(), tools, options)
    }

    fn with_route_and_options(
        provider: Box<dyn Provider>,
        route: ModelRoute,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        let behavior = BehaviorContract::from_runtime_options(&options);
        let mut registry = ToolRegistry::new();
        for tool in tools {
            registry.add(tool);
        }

        let plan = Arc::new(Mutex::new(Plan::new()));
        let planning_tool = UpdatePlanTool::with_plan(plan.clone());
        registry.add(Box::new(planning_tool));

        let save_plan_tool = SavePlanTool::with_plan(plan.clone());
        registry.add(Box::new(save_plan_tool));

        registry.add(Box::new(SaveLessonTool::new()));

        if behavior.generated_tools.authoring_enabled {
            registry.add(Box::new(CreateToolTool::new()));
            registry.add(Box::new(RepairToolTool::new()));
            registry.add(Box::new(ListGeneratedToolsTool::new()));
            registry.add(Box::new(DeleteGeneratedToolTool::new()));
        }

        Self {
            session: Session::new(),
            provider,
            tools: registry,
            external_tools: ExternalToolRegistry::new(),
            options,
            behavior,
            plan,
            hooks: HookRegistry::new(),
            current_objective: None,
            changed_files: RefCell::new(Vec::new()),
            active_files: RefCell::new(Vec::new()),
            bash_history: RefCell::new(Vec::new()),
            planning_gate_active: false,
            planning_required_for_task: false,
            task_mode: plan::TaskMode::PlanAndExecute,
            planning_escalated: false,
            resolved_route: route,
            execution_stage: ExecutionStage::Research,
            external_tool_ran: RefCell::new(false),
            run_baseline: RefCell::new(None),
            progress_callback: None,
            planning_block_count: 0,
            compaction_state: CompactionRuntimeState::default(),
            generated_tool_warnings: Vec::new(),
        }
    }

    pub fn plan(&self) -> Arc<Mutex<Plan>> {
        self.plan.clone()
    }

    pub fn hooks(&self) -> &HookRegistry {
        &self.hooks
    }

    pub fn hooks_mut(&mut self) -> &mut HookRegistry {
        &mut self.hooks
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.specs()
    }

    pub fn external_tools(&self) -> &ExternalToolRegistry {
        &self.external_tools
    }

    pub fn external_tools_mut(&mut self) -> &mut ExternalToolRegistry {
        &mut self.external_tools
    }

    pub fn changed_files(&self) -> Vec<String> {
        self.changed_files.borrow().clone()
    }

    pub fn conversation_messages(&self) -> Vec<Message> {
        self.session.raw_messages()
    }

    pub fn restore_conversation_messages(&mut self, messages: Vec<Message>) {
        self.session.replace_messages(messages);
    }

    pub fn set_progress_callback(&mut self, callback: Option<ProgressCallback>) {
        self.progress_callback = callback;
    }

    fn emit_progress(&self, update: ProgressUpdate) {
        if let Some(callback) = &self.progress_callback {
            callback(update);
        }
    }

    fn current_progress_phase(&self) -> &'static str {
        if self.planning_gate_active {
            return "planning";
        }

        match self.execution_stage {
            ExecutionStage::Research => "researching",
            ExecutionStage::Edit => "editing",
            ExecutionStage::Review => "verifying",
        }
    }

    fn current_working_progress(&self) -> ProgressUpdate {
        if self.planning_gate_active {
            return ProgressUpdate::planning();
        }

        match self.execution_stage {
            ExecutionStage::Research => ProgressUpdate::researching(),
            ExecutionStage::Edit => ProgressUpdate::editing(),
            ExecutionStage::Review => ProgressUpdate::verifying(),
        }
    }

    fn tool_progress(&self, name: &str, args: &serde_json::Value) -> ProgressUpdate {
        if self.behavior.is_planning_tool(name) {
            return ProgressUpdate::planning();
        }

        if name == "bash" {
            let bash_cmd = Self::extract_bash_command(args);
            return match Self::classify_bash_command(&bash_cmd) {
                BashCommandClass::Verification => {
                    ProgressUpdate::working("Running tool: bash (verification)".to_string())
                }
                _ => ProgressUpdate::running_tool("bash"),
            };
        }

        ProgressUpdate::running_tool(name)
    }

    fn blocked_progress(reason: &str) -> ProgressUpdate {
        if reason.contains("Planning required") {
            ProgressUpdate::blocked("Blocked: planning required before mutation.")
        } else {
            ProgressUpdate::blocked(format!("Blocked: {}", reason))
        }
    }

    fn stop_error() -> Error {
        Error::Stopped("user requested stop".to_string())
    }

    fn plan_exists(&self) -> bool {
        self.plan
            .lock()
            .map(|plan| !plan.is_empty())
            .unwrap_or(false)
    }

    fn deactivate_planning_gate(&mut self) {
        self.planning_gate_active = false;
        self.clear_planning_block_state();
    }

    fn maybe_compact_context(&mut self, ctx: &ExecutionContext) {
        let message_count = self.session.message_count();
        if !self.behavior.should_micro_compact(message_count) {
            return;
        }

        let snapshot = self.build_run_state_snapshot(ctx, self.plan_exists());
        let compactor = TranscriptCompactor::new(&self.behavior.compaction);

        if self.behavior.should_auto_compact(message_count) && !self.compaction_state.auto_disabled
        {
            match compactor.auto_compact(&mut self.session, &snapshot) {
                Ok(Some(_)) => {
                    self.compaction_state.consecutive_auto_failures = 0;
                }
                Ok(None) => {}
                Err(_) => {
                    self.compaction_state.consecutive_auto_failures += 1;
                    if self.compaction_state.consecutive_auto_failures
                        >= self.behavior.compaction.max_failed_auto_compactions
                    {
                        self.compaction_state.auto_disabled = true;
                    }
                    self.fallback_truncate_history();
                }
            }
            return;
        }

        let _ = compactor.micro_compact(&mut self.session, &snapshot);
        if self.compaction_state.auto_disabled {
            self.fallback_truncate_history();
        }
    }

    fn fallback_truncate_history(&mut self) {
        if self.session.message_count() <= self.behavior.compaction.max_messages_before_truncation {
            return;
        }

        let keep_recent = self.behavior.keep_recent_message_count();
        let notice = self
            .behavior
            .build_truncation_notice(self.session.message_count() - keep_recent);
        self.session
            .truncate_history_with_notice(keep_recent, move |_| notice);
    }

    /// Pop a previous redirect message (if present) and replace it with the
    /// model's response followed by a fresh redirect nudge.
    fn redirect_to_planning(&mut self, msg: Message, redirect_msg: &str) {
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
    fn generate_or_fallback_plan(
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

        // LLM failed — create a minimal emergency plan so the agent can proceed.
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

    fn note_planning_block(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<()> {
        if !self.planning_gate_active || self.plan_exists() {
            self.planning_block_count = 0;
            return Ok(());
        }

        self.planning_block_count += 1;
        if self.planning_block_count
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
    fn maybe_escalate_to_planning(&mut self) {
        let distinct_files = self.changed_files.borrow().len();
        if self.behavior.should_escalate_to_planning(
            self.planning_gate_active,
            self.planning_escalated,
            self.plan_exists(),
            distinct_files,
        ) {
            self.planning_gate_active = true;
            self.planning_required_for_task = true;
            self.planning_escalated = true;
            self.emit_progress(ProgressUpdate::planning());
        }
    }

    fn clear_planning_block_state(&mut self) {
        self.planning_block_count = 0;
    }

    #[allow(dead_code)]
    fn planning_still_blocked(&self) -> bool {
        self.planning_gate_active && !self.plan_exists() && self.planning_block_count > 0
    }

    fn check_cancelled(&self, ctx: &ExecutionContext) -> Result<()> {
        if ctx.is_cancelled() {
            return Err(Self::stop_error());
        }
        Ok(())
    }

    /// Record a tool call that produced `result` without successful execution
    /// (unknown tool, blocked by hook/gate, execution error).
    fn record_tool_result(
        &mut self,
        id: String,
        name: String,
        args: serde_json::Value,
        result: String,
    ) {
        self.session
            .add_message(Message::tool_request(id.clone(), name, args));
        self.session.add_message(Message::tool_result(id, result));
    }

    /// Shared preflight checks: pre-hooks, planning gate, pre-execution gate.
    /// Returns `None` if all pass, `Some(PreflightBlock)` if blocked.
    fn run_preflight(
        &mut self,
        ctx: &ExecutionContext,
        name: &str,
        args: &serde_json::Value,
        bash_args: Option<&serde_json::Value>,
        external_effect: Option<ExternalToolEffect>,
        external_sandbox: Option<CommandSandboxPolicy>,
    ) -> Result<Option<PreflightBlock>> {
        let tool_ctx = ToolContext::new(ctx, &self.options);
        if let Some(hooks) = self.hooks.get(name) {
            if !hooks.run_pre_hooks(name, args, &tool_ctx) {
                return Ok(Some(PreflightBlock {
                    message: "error: tool blocked by pre-hook".into(),
                    is_planning_block: false,
                }));
            }
        }

        if let Some(block_msg) = self.check_planning_gate(name, bash_args, external_effect) {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Ok(Some(PreflightBlock {
                message: block_msg,
                is_planning_block: true,
            }));
        }
        if let Some(block_msg) =
            self.check_pre_execution_verification_gate(name, bash_args, external_effect)
        {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Ok(Some(PreflightBlock {
                message: block_msg,
                is_planning_block: false,
            }));
        }
        if let Some(block) = self.check_approval_gate(
            ctx,
            name,
            args,
            bash_args,
            external_effect,
            external_sandbox,
        )? {
            return Ok(Some(block));
        }

        Ok(None)
    }

    /// Execute a single tool call (internal or external), updating session,
    /// planning gates, execution stage, and changed-file tracking.
    ///
    /// All early-exit paths (blocked, error, external-tool) return `Ok(())`
    /// after recording the appropriate tool_request / tool_result messages.
    /// Real errors (cancellation, planning-block escalation) propagate via `Err`.
    fn execute_single_tool_call(
        &mut self,
        ctx: &ExecutionContext,
        instruction: &str,
        id: String,
        name: String,
        args: serde_json::Value,
    ) -> Result<()> {
        let is_external = self.external_tools.get(&name).is_some();

        // ── Resolve tool (internal, external, or unknown) ──
        if self.tools.get(&name).is_none() {
            if is_external {
                return self.execute_external_tool_call(ctx, instruction, id, name, args);
            }
            self.record_tool_result(
                id,
                name.clone(),
                args,
                format!("error: unknown tool '{}'", name),
            );
            return Ok(());
        }

        // ── Preflight: hooks + gates ──
        self.track_active_file(&name, &args);
        let bash_args = if name == "bash" { Some(&args) } else { None };
        if let Some(block) = self.run_preflight(ctx, &name, &args, bash_args, None, None)? {
            self.record_tool_result(id, name, args, block.message);
            if block.is_planning_block {
                self.note_planning_block(ctx, instruction)?;
            }
            return Ok(());
        }

        let tool = self.tools.get(&name).unwrap();

        // ── Execute ──
        let tool_ctx = ToolContext::new(ctx, &self.options);
        let bash_cmd = if name == "bash" {
            Some(Self::extract_bash_command(&args))
        } else {
            None
        };
        self.emit_progress(self.tool_progress(&name, &args));
        self.check_cancelled(ctx)?;
        let mut result = match tool.execute(args.clone(), &tool_ctx) {
            Ok(r) => r,
            Err(e) => {
                self.record_tool_result(
                    id,
                    name,
                    args,
                    format!("error: tool execution failed: {}", e),
                );
                return Ok(());
            }
        };
        self.check_cancelled(ctx)?;

        // ── Bash post-processing ──
        let mut execution_started_by_bash = false;
        if name == "bash" {
            let class = if let Some(cmd_str) = &bash_cmd {
                Self::classify_bash_command(cmd_str)
            } else {
                BashCommandClass::MutationRisk
            };
            if let Some(cmd) = bash_cmd {
                let exit_code = extract_exit_code(&result);
                self.bash_history
                    .borrow_mut()
                    .push((cmd, result.clone(), exit_code));
            }
            if matches!(
                class,
                BashCommandClass::MutationRisk | BashCommandClass::Verification
            ) {
                let found_new_change = self.reconcile_changed_files(&ctx.workspace_root);
                if found_new_change {
                    execution_started_by_bash = true;
                }
            }
            if class == BashCommandClass::MutationRisk {
                execution_started_by_bash = true;
            }
        }

        // ── Post-hooks ──
        if let Some(hooks) = self.hooks.get(&name) {
            result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
        }

        if execution_started_by_bash {
            self.mark_execution_started();
        }

        // ── Track mutations ──
        self.track_changed_file(&name, &args);

        if self.behavior.is_mutation_tool(&name) {
            self.reconcile_changed_files(&ctx.workspace_root);
            self.mark_execution_started();
            self.maybe_escalate_to_planning();
        }

        if self.behavior.is_planning_tool(&name) && self.plan_exists() {
            self.deactivate_planning_gate();
        }

        if self.behavior.mutates_generated_tool_surface(&name) {
            self.reload_workspace_tools(&ctx.workspace_root)?;
        }

        // Redact secrets from tool output before it enters the
        // model context — defense-in-depth against exfiltration.
        let result = match ctx.secrets().redact(&result) {
            std::borrow::Cow::Owned(s) => s,
            std::borrow::Cow::Borrowed(_) => result,
        };

        self.record_tool_result(id, name, args, result);
        Ok(())
    }

    /// Handle an external tool call (preflight, execute, redact).
    fn execute_external_tool_call(
        &mut self,
        ctx: &ExecutionContext,
        instruction: &str,
        id: String,
        name: String,
        args: serde_json::Value,
    ) -> Result<()> {
        let external_effect = self.external_tools.get(&name).unwrap().effect();
        let external_sandbox = self.external_tools.get(&name).unwrap().sandbox_policy();

        // ── Preflight: hooks + gates ──
        if let Some(block) = self.run_preflight(
            ctx,
            &name,
            &args,
            None,
            Some(external_effect),
            Some(external_sandbox),
        )? {
            self.record_tool_result(id, name, args, block.message);
            if block.is_planning_block {
                self.note_planning_block(ctx, instruction)?;
            }
            return Ok(());
        }

        self.emit_progress(self.tool_progress(&name, &args));
        self.check_cancelled(ctx)?;
        let tool_ctx = ToolContext::new(ctx, &self.options);
        let external_tool = self.external_tools.get(&name).unwrap();
        let result = external_tool.execute(&args, &tool_ctx);
        self.check_cancelled(ctx)?;
        *self.external_tool_ran.borrow_mut() = true;

        let found_new_change = self.reconcile_changed_files(&ctx.workspace_root);
        if found_new_change && self.execution_stage == ExecutionStage::Research {
            self.execution_stage = ExecutionStage::Edit;
        }
        if found_new_change {
            self.maybe_escalate_to_planning();
        }

        let result_str = match result {
            Ok(r) => {
                if matches!(r.effect, ExternalToolEffect::ExecutionStarted) {
                    self.mark_execution_started();
                }
                r.output
            }
            Err(e) => {
                self.record_tool_result(
                    id,
                    name,
                    args,
                    format!("error: external tool execution failed: {}", e),
                );
                return Ok(());
            }
        };
        let result_str = match ctx.secrets().redact(&result_str) {
            std::borrow::Cow::Owned(s) => s,
            std::borrow::Cow::Borrowed(_) => result_str,
        };
        self.record_tool_result(id, name, args, result_str);
        Ok(())
    }

    fn track_changed_file(&self, tool_name: &str, args: &serde_json::Value) {
        if tool_name == "write" || tool_name == "edit" {
            if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                self.record_changed_file(path.to_string());
            }
        }
    }

    fn track_active_file(&self, tool_name: &str, args: &serde_json::Value) {
        let path = match tool_name {
            "read" | "write" | "edit" => args.get("path").and_then(|p| p.as_str()),
            _ => None,
        };
        let Some(path) = path else {
            return;
        };

        let mut active = self.active_files.borrow_mut();
        if let Some(existing) = active.iter().position(|entry| entry == path) {
            let entry = active.remove(existing);
            active.push(entry);
            return;
        }

        active.push(path.to_string());
        if active.len() > 12 {
            let excess = active.len() - 12;
            active.drain(0..excess);
        }
    }

    fn record_changed_file(&self, path: String) {
        if self.is_pre_existing_dirty(&path) {
            return;
        }
        let mut changed = self.changed_files.borrow_mut();
        if !changed.contains(&path) {
            changed.push(path);
        }
    }

    fn build_run_state_snapshot(
        &self,
        ctx: &ExecutionContext,
        plan_exists: bool,
    ) -> RunStateSnapshot {
        let mut blockers = Vec::new();
        if self.planning_gate_active && !plan_exists {
            blockers
                .push("Planning required before mutation-risk actions can continue.".to_string());
        }

        let mut pending_approvals = Vec::new();
        let mut recent_approval_decisions = Vec::new();
        if let Some(mailbox) = ctx.approval_mailbox() {
            for entry in mailbox.pending() {
                pending_approvals.push(entry.request.render_status_line(entry.state));
            }

            let mut resolved = mailbox
                .list()
                .into_iter()
                .filter(|entry| entry.state != crate::approval::ApprovalState::Pending)
                .collect::<Vec<_>>();
            resolved.sort_by_key(|entry| entry.resolved_at.or(Some(entry.request.created_at)));

            for entry in resolved
                .into_iter()
                .rev()
                .take(self.behavior.compaction.max_recent_approval_decisions)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                let mut line = entry.request.render_status_line(entry.state);
                if let Some(note) = entry.decision_note {
                    line.push_str(&format!(" ({note})"));
                }
                if matches!(
                    entry.state,
                    crate::approval::ApprovalState::Denied
                        | crate::approval::ApprovalState::Expired
                        | crate::approval::ApprovalState::Superseded
                ) {
                    blockers.push(format!(
                        "Approval {}: {}",
                        entry.state.label(),
                        entry.request.short_summary
                    ));
                }
                recent_approval_decisions.push(line);
            }
        }

        let mut active_files = self.active_files.borrow().clone();
        for changed in self.changed_files.borrow().iter() {
            if !active_files.contains(changed) {
                active_files.push(changed.clone());
            }
        }

        let changed_files = self.changed_files.borrow().clone();
        let mut proof_of_work_anchors = Vec::new();
        if !changed_files.is_empty() {
            proof_of_work_anchors.push(format!("changed files: {}", changed_files.join(", ")));
        }

        for (command, _output, exit_code) in self.bash_history.borrow().iter().rev() {
            if !self.behavior.is_verification_command(command) {
                continue;
            }
            proof_of_work_anchors.push(format!("verification: {} (exit {})", command, exit_code));
            if proof_of_work_anchors.len()
                >= self.behavior.compaction.max_recent_proof_of_work_anchors
            {
                break;
            }
        }
        proof_of_work_anchors.reverse();

        RunStateSnapshot {
            objective: self.current_objective.clone(),
            blockers,
            pending_approvals,
            recent_approval_decisions,
            active_files,
            proof_of_work_anchors,
            memory_context_loaded: ctx.memory_context().is_some(),
        }
    }

    pub fn get_route(&self) -> ModelRoute {
        match self.execution_stage {
            ExecutionStage::Edit => {
                if let Some(ref model) = self.options.edit_model {
                    return ModelRoute::openrouter(model);
                }
            }
            ExecutionStage::Review => {
                if let Some(ref model) = self.options.review_model {
                    return ModelRoute::openrouter(model);
                }
            }
            ExecutionStage::Research => {
                if self.planning_gate_active {
                    if let Some(ref model) = self.options.research_model {
                        return ModelRoute::openrouter(model);
                    }
                }
            }
        }
        self.resolved_route.clone()
    }

    pub fn set_execution_stage(&mut self, stage: ExecutionStage) {
        self.execution_stage = stage;
    }

    pub fn execution_stage(&self) -> ExecutionStage {
        self.execution_stage
    }

    pub fn is_planning_gate_active(&self) -> bool {
        self.planning_gate_active
    }

    fn execution_started(&self) -> bool {
        self.execution_stage != ExecutionStage::Research
    }

    fn mark_execution_started(&mut self) {
        if self.execution_stage == ExecutionStage::Research {
            self.execution_stage = ExecutionStage::Edit;
        }
    }

    fn check_pre_execution_verification_gate(
        &self,
        tool_name: &str,
        bash_args: Option<&serde_json::Value>,
        external_effect: Option<ExternalToolEffect>,
    ) -> Option<String> {
        let bash_command = bash_args
            .and_then(|args| args.get("command"))
            .and_then(|value| value.as_str());

        self.behavior.pre_execution_block_message(
            tool_name,
            bash_command,
            external_effect,
            &PreExecutionState {
                planning_required_for_task: self.planning_required_for_task,
                plan_exists: self.plan_exists(),
                execution_started: self.execution_started(),
                task_mode: self.task_mode,
            },
        )
    }

    fn check_approval_gate(
        &self,
        ctx: &ExecutionContext,
        tool_name: &str,
        args: &serde_json::Value,
        bash_args: Option<&serde_json::Value>,
        external_effect: Option<ExternalToolEffect>,
        external_sandbox: Option<CommandSandboxPolicy>,
    ) -> Result<Option<PreflightBlock>> {
        let Some(mailbox) = ctx.approval_mailbox() else {
            return Ok(None);
        };

        let bash_command = bash_args
            .and_then(|args| args.get("command"))
            .and_then(|value| value.as_str());
        let Some(request) = self.behavior.approval_request(
            tool_name,
            args,
            bash_command,
            external_effect,
            external_sandbox,
        ) else {
            return Ok(None);
        };

        let blocked_message = format!("approval required for {}", request.short_summary);
        self.emit_progress(Self::blocked_progress(&blocked_message));
        match mailbox.request_decision(request, ctx.cancel_token()) {
            ApprovalCheck::Approved(_) => Ok(None),
            ApprovalCheck::Pending(entry) => Err(Error::ApprovalRequired(Box::new(entry.request))),
            ApprovalCheck::Denied(entry) => Ok(Some(PreflightBlock {
                message: format!("error: approval denied for {}", entry.request.short_summary),
                is_planning_block: false,
            })),
            ApprovalCheck::Expired(entry) => Ok(Some(PreflightBlock {
                message: format!(
                    "error: approval expired for {}",
                    entry.request.short_summary
                ),
                is_planning_block: false,
            })),
            ApprovalCheck::Superseded(entry) => Ok(Some(PreflightBlock {
                message: format!(
                    "error: approval superseded for {}",
                    entry.request.short_summary
                ),
                is_planning_block: false,
            })),
        }
    }

    fn compute_file_hash(path: &Path) -> Option<String> {
        use std::collections::hash_map::DefaultHasher;
        use std::fs::File;
        use std::hash::{Hash, Hasher};
        use std::io::Read;

        let mut file = File::open(path).ok()?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).ok()?;
        let mut hasher = DefaultHasher::new();
        contents.hash(&mut hasher);
        Some(format!("{:x}", hasher.finish()))
    }

    fn capture_run_baseline(&self, workspace_root: &Path) {
        let dirty = Self::list_dirty_files(workspace_root);
        let mut hashes = HashMap::new();
        let mut unattributed = Vec::new();

        for file in &dirty {
            let path = workspace_root.join(file);
            if let Some(hash) = Self::compute_file_hash(&path) {
                hashes.insert(file.clone(), hash);
            } else {
                unattributed.push(file.clone());
            }
        }

        *self.run_baseline.borrow_mut() = Some(RunBaseline {
            pre_existing_dirty: dirty,
            pre_existing_hashes: hashes,
            pre_existing_unattributed: unattributed,
        });
    }

    fn reconcile_changed_files(&self, workspace_root: &Path) -> bool {
        let baseline = self.run_baseline.borrow();
        let pre_existing_dirty = baseline
            .as_ref()
            .map_or(vec![], |b| b.pre_existing_dirty.clone());
        let pre_existing_hashes = baseline
            .as_ref()
            .map_or(HashMap::new(), |b| b.pre_existing_hashes.clone());
        let current_dirty = Self::list_dirty_files(workspace_root);

        let mut changed = self.changed_files.borrow_mut();
        let mut found_new_change = false;

        for file in current_dirty {
            let was_pre_existing = pre_existing_dirty.contains(&file);

            if was_pre_existing {
                if let Some(baseline_hash) = pre_existing_hashes.get(&file) {
                    let current_hash = Self::compute_file_hash(&workspace_root.join(&file));
                    if current_hash.as_ref() != Some(baseline_hash) {
                        if !changed.contains(&file) {
                            changed.push(file.clone());
                        }
                        found_new_change = true;
                    }
                }
            } else {
                if !changed.contains(&file) {
                    changed.push(file.clone());
                }
                found_new_change = true;
            }
        }

        found_new_change
    }

    fn is_pre_existing_dirty(&self, path: &str) -> bool {
        let baseline = self.run_baseline.borrow();
        baseline
            .as_ref()
            .is_some_and(|b| b.pre_existing_dirty.iter().any(|file| file == path))
    }

    fn unattributed_pre_existing_dirty_files(&self, workspace_root: &Path) -> Vec<String> {
        let baseline = self.run_baseline.borrow();
        let Some(baseline) = baseline.as_ref() else {
            return Vec::new();
        };

        if baseline.pre_existing_unattributed.is_empty() {
            return Vec::new();
        }

        let current_dirty = Self::list_dirty_files(workspace_root);
        baseline
            .pre_existing_unattributed
            .iter()
            .filter(|file| current_dirty.contains(file))
            .cloned()
            .collect()
    }

    fn list_dirty_files(workspace_root: &Path) -> Vec<String> {
        let mut dirty = Vec::new();

        if let Ok(output) = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .current_dir(workspace_root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    dirty.push(trimmed.to_string());
                }
            }
        }

        if let Ok(output) = std::process::Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(workspace_root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !dirty.contains(&trimmed.to_string()) {
                    dirty.push(trimmed.to_string());
                }
            }
        }

        dirty
    }

    pub fn classify_bash_command(cmd: &str) -> BashCommandClass {
        BehaviorContract::default().classify_bash_command(cmd)
    }

    fn check_planning_gate(
        &self,
        tool_name: &str,
        bash_args: Option<&serde_json::Value>,
        external_effect: Option<ExternalToolEffect>,
    ) -> Option<String> {
        if !self.planning_gate_active {
            return None;
        }
        let bash_command = bash_args
            .and_then(|args| args.get("command"))
            .and_then(|value| value.as_str());

        self.behavior.planning_block_message(
            tool_name,
            bash_command,
            external_effect,
            self.plan_exists(),
        )
    }

    fn extract_bash_command(args: &serde_json::Value) -> String {
        args.get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    pub fn load_workspace_external_tools(&mut self, workspace_root: &Path) -> Result<()> {
        let path = workspace_root.join(WORKSPACE_EXTERNAL_TOOLS_PATH);
        if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(Error::Io)?;
            self.external_tools.load_from_str(&content)?;
        }
        Ok(())
    }

    pub fn load_generated_tools_from_workspace(&mut self, workspace_root: &Path) -> Result<()> {
        let genesis = ToolGenesis::new(workspace_root.to_path_buf());
        let inventory = genesis.generated_tool_inventory()?;
        self.generated_tool_warnings = inventory
            .summaries
            .iter()
            .filter_map(|summary| {
                summary
                    .load_warning
                    .as_ref()
                    .map(|warning| format!("{}: {}", summary.name, warning))
            })
            .collect();
        for tool in inventory.verified_tools {
            self.external_tools.register(tool);
        }
        Ok(())
    }

    pub fn run(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.emit_progress(ProgressUpdate::received());

        let result = self.run_inner(ctx, instruction);
        match &result {
            Ok(_) => self.emit_progress(ProgressUpdate::completed()),
            Err(Error::Stopped(_)) => self.emit_progress(ProgressUpdate::stopped()),
            Err(err) => self.emit_progress(ProgressUpdate::failed(err.to_string())),
        }
        result
    }

    fn run_inner(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.check_cancelled(ctx)?;
        self.reset_run_state(ctx, instruction)?;
        self.reload_workspace_tools(&ctx.workspace_root)?;
        self.emit_progress(self.current_working_progress());

        self.session.add_message(Message::user(instruction));

        let mut steps = 0;
        let mut empty_response_retries = 0;
        let mut planning_phase_steps = 0usize;
        let mut planning_redirects = 0usize;
        let mut provider_msgs = Vec::new();

        loop {
            self.check_cancelled(ctx)?;
            if steps >= self.options.max_steps {
                return Err(Error::MaxStepsReached(format!(
                    "max steps ({}) reached without completing task",
                    self.options.max_steps
                )));
            }

            self.maybe_compact_context(ctx);
            if self.behavior.compaction.refresh_system_prompt_each_turn || steps == 0 {
                self.session
                    .set_system_prompt(&self.build_run_system_prompt(ctx)?);
            }

            // Planning phase budget: if the model spent too many steps
            // researching without creating a plan, generate one.
            if self.planning_gate_active && !self.plan_exists() {
                planning_phase_steps += 1;
                if planning_phase_steps >= self.behavior.planning.max_research_steps_without_plan {
                    self.generate_or_fallback_plan(instruction, ctx.cancel_token())?;
                    self.emit_progress(self.current_working_progress());
                }
            }

            self.emit_progress(ProgressUpdate::waiting_for_model(
                self.current_progress_phase(),
            ));
            self.session.fill_messages(&mut provider_msgs);
            let response = match self.provider.complete_with_cancel(
                &provider_msgs,
                &self.get_route(),
                ctx.cancel_token(),
            ) {
                Ok(r) => {
                    self.check_cancelled(ctx)?;
                    r
                }
                Err(e) => {
                    if ctx.is_cancelled() {
                        return Err(Self::stop_error());
                    }
                    if empty_response_retries >= self.options.max_provider_retries {
                        return Err(Error::ProviderRetryExhausted(format!(
                            "provider failed after {} retries: {}",
                            self.options.max_provider_retries, e
                        )));
                    }
                    empty_response_retries += 1;
                    if empty_response_retries >= self.options.max_provider_retries {
                        return Err(Error::ProviderRetryExhausted(format!(
                            "provider failed repeatedly ({} attempts): {}",
                            empty_response_retries, e
                        )));
                    }
                    self.emit_progress(ProgressUpdate::retrying_provider(
                        empty_response_retries,
                        self.options.max_provider_retries,
                    ));
                    continue;
                }
            };

            steps += 1;

            match response {
                ProviderResponse::Message(msg) => {
                    let text = msg.as_text().map(|s| s.to_string());
                    if let Some(text) = text {
                        if text.is_empty() {
                            if empty_response_retries >= self.options.max_provider_retries {
                                return Err(Error::ProviderRetryExhausted(
                                    "provider returned empty response after max retries".into(),
                                ));
                            }
                            empty_response_retries += 1;
                            self.emit_progress(ProgressUpdate::retrying_empty_response(
                                empty_response_retries,
                                self.options.max_provider_retries,
                            ));
                            continue;
                        }
                        // If the planning gate is active and no plan exists, the model
                        // is trying to return a text response without creating a plan.
                        // Redirect it back to plan instead of accepting as final answer.
                        if self.planning_gate_active && !self.plan_exists() {
                            planning_redirects += 1;
                            if planning_redirects
                                >= self.behavior.planning.max_text_redirects_before_auto_plan
                            {
                                // Model repeatedly refused to plan — generate one.
                                self.generate_or_fallback_plan(instruction, ctx.cancel_token())?;
                                self.emit_progress(self.current_working_progress());
                            }
                            self.redirect_to_planning(msg, self.behavior.planning.redirect_message);
                            continue;
                        }

                        self.session.add_message(msg);
                        let final_response = self.build_proof_of_work(&text, &ctx.workspace_root);
                        return Ok(final_response);
                    }
                    self.session.add_message(msg);
                }
                ProviderResponse::ToolCall { id, name, args } => {
                    self.execute_single_tool_call(ctx, instruction, id, name, args)?;
                    empty_response_retries = 0;
                }
                ProviderResponse::ToolCalls(calls) => {
                    for call in calls {
                        self.execute_single_tool_call(
                            ctx,
                            instruction,
                            call.id,
                            call.name,
                            call.args,
                        )?;
                    }
                    empty_response_retries = 0;
                }
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
    }

    fn reload_workspace_tools(&mut self, workspace_root: &Path) -> Result<()> {
        self.external_tools = ExternalToolRegistry::new();
        self.generated_tool_warnings.clear();
        self.load_workspace_external_tools(workspace_root)?;
        self.load_generated_tools_from_workspace(workspace_root)?;
        self.sync_provider_tools();
        Ok(())
    }

    fn sync_provider_tools(&mut self) {
        let mut tool_specs = self.tools.specs();
        tool_specs.extend(self.external_tools.specs());
        self.provider.set_tool_specs(tool_specs);
    }

    fn reset_run_state(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<()> {
        let workspace_root = &ctx.workspace_root;
        self.current_objective = Some(instruction.to_string());
        self.changed_files.borrow_mut().clear();
        self.active_files.borrow_mut().clear();
        self.bash_history.borrow_mut().clear();
        *self.external_tool_ran.borrow_mut() = false;
        self.compaction_state = CompactionRuntimeState::default();
        self.capture_run_baseline(workspace_root);

        self.planning_required_for_task = self.behavior.planning.require_plan_by_default
            && self.classify_task(instruction, ctx.cancel_token())?;
        self.task_mode = if self.planning_required_for_task {
            self.classify_task_mode(instruction, ctx.cancel_token())?
        } else {
            plan::TaskMode::PlanAndExecute
        };
        self.planning_gate_active = self.planning_required_for_task;
        self.planning_escalated = false;
        self.planning_block_count = 0;
        self.execution_stage = ExecutionStage::Research;
        Ok(())
    }

    fn build_run_system_prompt(&self, ctx: &ExecutionContext) -> Result<String> {
        let project_instructions = get_project_instructions_or_error(&ctx.workspace_root)?;
        let available_tools = self.tools.specs();
        let external_tools = self.external_tools.specs();
        let plan_guard = self.plan.lock().ok();
        let current_plan = plan_guard
            .as_ref()
            .filter(|plan| !plan.is_empty())
            .map(|plan| &**plan);
        let plan_exists = current_plan.is_some();
        let run_state = self.build_run_state_snapshot(ctx, plan_exists);

        Ok(self.behavior.render_system_prompt(&BehaviorPromptContext {
            available_tools: &available_tools,
            external_tools: &external_tools,
            project_instructions: project_instructions.as_deref(),
            memory_context: ctx.memory_context(),
            current_plan,
            run_state: Some(&run_state),
            generated_tool_warnings: &self.generated_tool_warnings,
            planning_required_now: self.planning_gate_active && !plan_exists,
            approval_mailbox_available: ctx.approval_mailbox().is_some(),
        }))
    }

    fn generated_tool_warning_lines(&self) -> &[String] {
        &self.generated_tool_warnings
    }

    fn build_proof_of_work(&self, response: &str, workspace_root: &Path) -> String {
        let files = self.changed_files.borrow().clone();
        let unattributed_files = self.unattributed_pre_existing_dirty_files(workspace_root);
        let baseline = self.run_baseline.borrow();
        let pre_existing = baseline
            .as_ref()
            .map_or(vec![], |b| b.pre_existing_dirty.clone());
        let labeled_files: Vec<String> = files
            .iter()
            .map(|f| {
                if pre_existing.contains(f) {
                    format!("{} (pre-existing dirty, changed again during this run)", f)
                } else {
                    f.clone()
                }
            })
            .collect();

        let diff_summary = if !files.is_empty() {
            Self::generate_diff_summary(workspace_root, &files)
        } else {
            String::new()
        };

        let mut evidence = TaskEvidence {
            files_changed: labeled_files,
            diff_summary,
            verification_commands_run: Vec::new(),
            unresolved_issues: Vec::new(),
            workspace_warnings: Vec::new(),
        };

        for (command, full_output, exit_code) in self.bash_history.borrow().iter() {
            if self.behavior.is_verification_command(command) {
                let succeeded = exit_code == &0;
                evidence
                    .verification_commands_run
                    .push(VerificationCommand {
                        command: command.clone(),
                        output: full_output.clone(),
                        exit_code: *exit_code,
                        succeeded,
                    });
            }
        }

        if !files.is_empty() && evidence.verification_commands_run.is_empty() {
            evidence
                .unresolved_issues
                .push("Files were modified but no verification commands were run".to_string());
        }

        if !unattributed_files.is_empty() {
            let details = unattributed_files
                .iter()
                .map(|file| {
                    format!(
                        "{} (pre-existing dirty file; baseline unavailable, run attribution uncertain)",
                        file
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            evidence
                .unresolved_issues
                .push(format!("Attribution uncertain: {}", details));
        }

        if !self.behavior.should_attach_proof_of_work(
            evidence.files_changed.len(),
            evidence.verification_commands_run.len(),
            evidence.unresolved_issues.len(),
            self.generated_tool_warning_lines().len(),
        ) {
            return response.to_string();
        }

        let task_result = TaskResult::new(response.to_string())
            .with_files_changed(evidence.files_changed.clone())
            .with_diff_summary(evidence.diff_summary.clone())
            .with_verification_commands(evidence.verification_commands_run.clone())
            .with_unresolved_issues(evidence.unresolved_issues.clone())
            .with_workspace_warnings(self.generated_tool_warning_lines().to_vec());

        task_result.format_proof_of_work()
    }

    fn generate_diff_summary(workspace_root: &Path, changed_files: &[String]) -> String {
        if changed_files.is_empty() {
            return String::new();
        }
        let mut summary_parts = Vec::new();
        for file in changed_files {
            // Check if file is untracked (new file)
            let is_untracked = std::process::Command::new("git")
                .args(["ls-files", "--others", "--exclude-standard", file])
                .current_dir(workspace_root)
                .output()
                .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
                .unwrap_or(false);

            if is_untracked {
                // For new files, show the content as added
                if let Ok(content) = std::fs::read_to_string(workspace_root.join(file)) {
                    let line_count = content.lines().count();
                    summary_parts.push(format!("{}: {} lines added", file, line_count));
                } else {
                    summary_parts.push(format!("{}: (new file)", file));
                }
            } else {
                // For modified files, show git diff stat
                let output = std::process::Command::new("git")
                    .args(["diff", "--stat", file])
                    .current_dir(workspace_root)
                    .output();

                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        if !stdout.trim().is_empty() {
                            summary_parts.push(stdout.to_string());
                        } else if !stderr.trim().is_empty() {
                            summary_parts.push(format!("{}: (no diff)", file));
                        } else {
                            summary_parts.push(format!("{}: (unchanged)", file));
                        }
                    }
                    Err(e) => {
                        summary_parts.push(format!("{}: (diff unavailable: {})", file, e));
                    }
                }
            }
        }
        summary_parts.join("\n")
    }
}

fn extract_exit_code(result: &str) -> i32 {
    let prefix = "\nExit code: ";
    if let Some(pos) = result.find(prefix) {
        let after_prefix = &result[pos + prefix.len()..];
        after_prefix
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect::<String>()
            .parse()
            .unwrap_or(-1)
    } else {
        // Missing prefix means truncated or malformed output — do not assume success.
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_exit_code, Agent};
    use crate::approval::{
        ApprovalMailbox, ApprovalMailboxMode, ApprovalRequestDraft, ApprovalTriggerKind,
    };
    use crate::context::ExecutionContext;
    use crate::provider::{ProviderResponse, ScriptedProvider};
    use crate::runtime::RuntimeOptions;
    use crate::tools::default_tools;
    use crate::{Error, Message};
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_temp_crate() -> (TempDir, ExecutionContext) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"[package]
name = "stage_gate_fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n",
        )
        .unwrap();

        (temp, ExecutionContext::new(root))
    }

    fn make_plan_required_agent(responses: Vec<ProviderResponse>) -> Agent {
        Agent::with_options(
            Box::new(ScriptedProvider::new(responses)),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        )
    }

    fn assistant_message(text: &str) -> ProviderResponse {
        ProviderResponse::Message(Message::assistant(text))
    }

    fn task_mode_message(mode: &str) -> ProviderResponse {
        assistant_message(mode)
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ProviderResponse {
        ProviderResponse::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    fn update_plan_call(id: &str) -> ProviderResponse {
        tool_call(
            id,
            "update_plan",
            serde_json::json!({
                "items": [
                    {"content": "Edit src/lib.rs", "status": "in_progress"},
                    {"content": "Run cargo check --offline", "status": "pending"}
                ]
            }),
        )
    }

    fn write_lib_call(id: &str, content: &str) -> ProviderResponse {
        tool_call(
            id,
            "write",
            serde_json::json!({
                "path": "src/lib.rs",
                "content": content,
            }),
        )
    }

    fn cargo_check_call(id: &str) -> ProviderResponse {
        tool_call(
            id,
            "bash",
            serde_json::json!({
                "command": "cargo check --offline",
            }),
        )
    }

    fn write_workspace_external_tools_json(temp: &TempDir, entries: serde_json::Value) {
        let topagent_dir = temp.path().join(".topagent");
        fs::create_dir_all(&topagent_dir).unwrap();
        fs::write(
            topagent_dir.join("external-tools.json"),
            serde_json::to_string(&entries).unwrap(),
        )
        .unwrap();
    }

    fn run_git(workspace: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn create_temp_git_repo() -> (TempDir, ExecutionContext) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(temp.path().join("tracked.txt"), "before\n").unwrap();
        run_git(temp.path(), &["init"]);
        run_git(
            temp.path(),
            &["config", "user.email", "topagent@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "TopAgent"]);
        run_git(temp.path(), &["add", "tracked.txt"]);
        run_git(temp.path(), &["commit", "-m", "initial"]);
        fs::write(temp.path().join("tracked.txt"), "after\n").unwrap();
        run_git(temp.path(), &["add", "tracked.txt"]);
        (temp, ExecutionContext::new(root))
    }

    fn seed_mailbox_for_compaction_test(mailbox: &ApprovalMailbox) {
        let pending = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::GitCommit,
                short_summary: "git commit: release snapshot".to_string(),
                exact_action: "git_commit(message=\"release snapshot\")".to_string(),
                reason: "commits publish a durable repo milestone".to_string(),
                scope_of_impact: "Creates a new git commit in the workspace repository."
                    .to_string(),
                expected_effect: "Staged changes become a durable commit.".to_string(),
                rollback_hint: Some(
                    "Use git revert or git reset if the commit was mistaken.".to_string(),
                ),
            },
            None,
        );
        let denied = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::GeneratedToolDeletion,
                short_summary: "delete generated tool: cleanup_helper".to_string(),
                exact_action: "delete_generated_tool(name=\"cleanup_helper\")".to_string(),
                reason: "tool deletion removes workspace-local capability".to_string(),
                scope_of_impact: "Deletes a generated tool from .topagent/tools.".to_string(),
                expected_effect: "The generated helper disappears from the callable tool surface."
                    .to_string(),
                rollback_hint: Some("Recreate the tool if the deletion was mistaken.".to_string()),
            },
            None,
        );

        let denied_id = match denied {
            crate::approval::ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending approval entry, got {other:?}"),
        };
        mailbox
            .deny(&denied_id, Some("keep the helper around".to_string()))
            .unwrap();

        match pending {
            crate::approval::ApprovalCheck::Pending(_) => {}
            other => panic!("expected pending approval entry, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_exit_code_zero() {
        assert_eq!(extract_exit_code("Output: hello\nExit code: 0"), 0);
    }

    #[test]
    fn test_extract_exit_code_nonzero() {
        assert_eq!(extract_exit_code("Stderr: err\nExit code: 1"), 1);
        assert_eq!(extract_exit_code("Output: x\nExit code: 127"), 127);
    }

    #[test]
    fn test_extract_exit_code_no_prefix_defaults_to_failure() {
        assert_eq!(extract_exit_code("some random output"), -1);
    }

    #[test]
    fn test_extract_exit_code_negative() {
        assert_eq!(extract_exit_code("Output: x\nExit code: -1"), -1);
    }

    #[test]
    fn test_inspection_only_task_does_not_get_blocked_unnecessarily() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            task_mode_message("inspect"),
            update_plan_call("plan"),
            assistant_message("assessment complete"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan to assess this codebase and return findings only.",
            )
            .unwrap();

        assert_eq!(result, "assessment complete");
    }

    #[test]
    fn test_verification_only_task_does_not_get_blocked_unnecessarily() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            task_mode_message("verify"),
            update_plan_call("plan"),
            cargo_check_call("verify"),
            assistant_message("validation complete"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan to validate this crate and report the result only.",
            )
            .unwrap();

        assert!(result.contains("validation complete"));
        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
    }

    #[test]
    fn test_plan_required_task_cannot_verify_before_plan_exists() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            cargo_check_call("verify_before_plan"),
            update_plan_call("plan"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    43\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
        assert!(result.contains("Verification passed."));
    }

    #[test]
    fn test_plan_required_task_cannot_verify_before_execution_happened() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            cargo_check_call("verify_before_execution"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    44\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
        assert!(result.contains("- src/lib.rs"));
    }

    #[test]
    fn test_plan_required_task_can_verify_after_execution_happened() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    45\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after verification"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert!(result.contains("done after verification"));
        assert!(result.contains("Verification passed."));
        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
    }

    #[test]
    fn test_text_response_accepted_after_plan_creation() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            assistant_message("done before plan"),
            update_plan_call("plan"),
            assistant_message("done after plan"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        // Text response before plan should be redirected; text after plan is accepted.
        assert!(result.starts_with("done after plan"));
        assert!(!result.contains("done before plan"));
    }

    #[test]
    fn test_external_tool_execution_effect_unlocks_verification_without_diff() {
        let (temp, ctx) = create_temp_crate();
        write_workspace_external_tools_json(
            &temp,
            serde_json::json!([
                {
                    "name": "start_exec",
                    "description": "mark execution started",
                    "command": "true",
                    "argv_template": [],
                    "sandbox": "host",
                    "effect": "execution_started"
                }
            ]),
        );

        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            tool_call("exec", "start_exec", serde_json::json!({})),
            cargo_check_call("verify"),
            assistant_message("done after external execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert!(result.contains("done after external execution"));
        assert!(result.contains("Verification passed."));
        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
    }

    #[test]
    fn test_verification_remains_blocked_without_real_execution_signal() {
        let (temp, ctx) = create_temp_crate();
        write_workspace_external_tools_json(
            &temp,
            serde_json::json!([
                {
                    "name": "read_tool",
                    "description": "read-only helper",
                    "command": "true",
                    "argv_template": [],
                    "sandbox": "host",
                    "effect": "read_only"
                }
            ]),
        );

        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            tool_call("read", "read_tool", serde_json::json!({})),
            cargo_check_call("verify_before_execution"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    47\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after real execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
        assert!(result.contains("done after real execution"));
    }

    #[test]
    fn test_git_commit_requires_approval_and_does_not_execute_silently() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let provider = ScriptedProvider::new(vec![tool_call(
            "commit",
            "git_commit",
            serde_json::json!({"message": "ship it"}),
        )]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "commit the staged change");
        let request = match result {
            Err(Error::ApprovalRequired(request)) => request,
            other => panic!("expected approval-required error, got {other:?}"),
        };

        assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
        let commit_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&ctx.workspace_root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&commit_count.stdout).trim(), "1");
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_host_external_execution_requires_approval_and_does_not_run() {
        let (temp, ctx) = create_temp_crate();
        write_workspace_external_tools_json(
            &temp,
            serde_json::json!([
                {
                    "name": "host_touch",
                    "description": "create a host-side marker file",
                    "command": "bash",
                    "argv_template": ["-lc", "touch host-risk.txt"],
                    "sandbox": "host",
                    "effect": "execution_started"
                }
            ]),
        );

        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let provider =
            ScriptedProvider::new(vec![tool_call("host", "host_touch", serde_json::json!({}))]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "run the host helper");
        let request = match result {
            Err(Error::ApprovalRequired(request)) => request,
            other => panic!("expected approval-required error, got {other:?}"),
        };

        assert_eq!(
            request.action_kind,
            ApprovalTriggerKind::HostExternalExecution
        );
        assert!(!ctx.workspace_root.join("host-risk.txt").exists());
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_approved_git_commit_executes_through_waiting_mailbox() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let mailbox_for_notifier = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            mailbox_for_notifier
                .approve(&request.id, Some("approved in test".to_string()))
                .unwrap();
        }));
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let provider = ScriptedProvider::new(vec![
            tool_call(
                "commit",
                "git_commit",
                serde_json::json!({"message": "ship it"}),
            ),
            assistant_message("commit complete"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "commit the staged change").unwrap();

        assert!(result.contains("commit complete"));
        let commit_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&ctx.workspace_root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&commit_count.stdout).trim(), "2");
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            crate::approval::ApprovalState::Approved
        );
    }

    #[test]
    fn test_compaction_preserves_objective_plan_and_approval_state_in_prompt_rebuild() {
        let (_temp, ctx) = create_temp_crate();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        seed_mailbox_for_compaction_test(&mailbox);
        let ctx = ctx.with_approval_mailbox(mailbox);
        let provider = ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            tool_call("read-2", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("done"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default().with_max_messages_before_truncation(4),
        );

        let instruction = "Refactor the entire codebase safely after you make a plan.";
        let result = agent.run(&ctx, instruction).unwrap();

        assert_eq!(result, "done");
        let prompt = agent.build_run_system_prompt(&ctx).unwrap();
        assert!(prompt.contains("## Active Run State"));
        assert!(prompt.contains(instruction));
        assert!(prompt.contains("## Current Plan"));
        assert!(prompt.contains("apr-1 [pending] git commit: release snapshot"));
        assert!(prompt.contains("apr-2 [denied] delete generated tool: cleanup_helper"));
        assert!(prompt.contains("Approval denied: delete generated tool: cleanup_helper"));

        let summary = agent
            .session
            .raw_messages()
            .into_iter()
            .find_map(|message| {
                message
                    .as_text()
                    .filter(|text| text.starts_with("["))
                    .map(str::to_string)
            })
            .expect("compaction summary should be present");
        assert!(summary.contains(instruction));
        assert!(summary.contains("current plan"));
        assert!(summary.contains("apr-2 [denied] delete generated tool: cleanup_helper"));
    }
}

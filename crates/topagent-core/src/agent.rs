use crate::context::{ExecutionContext, ToolContext};
use crate::external::{ExternalToolEffect, ExternalToolRegistry};
use crate::hooks::HookRegistry;
use crate::model::ModelRoute;
use crate::plan::{self, Plan};
use crate::progress::{ProgressCallback, ProgressUpdate};
use crate::project::get_project_instructions_or_error;
use crate::prompt;
use crate::runtime::RuntimeOptions;
use crate::session::Session;
use crate::task_result::{TaskEvidence, TaskResult, VerificationCommand};
use crate::tool_genesis::{
    ApproveToolProposalTool, CreateToolTool, DeleteGeneratedToolTool, DesignToolTool,
    ImplementToolProposalTool, ListGeneratedToolsTool, ListToolProposalsTool,
    RejectToolProposalTool, RepairToolTool, ReviseToolProposalTool, ShowToolProposalTool,
    ToolGenesis,
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
// 1. `planning_block_count` (vs MAX_PLANNING_BLOCKS_BEFORE_FAILURE):
//    Counts consecutive mutation-tool calls blocked by the planning gate.
//    Covers: model actively tries to mutate without creating a plan.
//
// 2. `planning_phase_steps` (vs MAX_PLANNING_PHASE_STEPS):
//    Counts total loop iterations while gate is active and plan is empty.
//    Covers: model loops in research tools without ever attempting mutation
//    or planning.
//
// Both trigger the same fallback: try a dedicated LLM plan-generation call,
// and if that fails, create a minimal emergency plan.
//
// `planning_redirects` (vs MAX_PLANNING_REDIRECTS):
//    Counts text-response bail-outs during planning phase.
//    Covers: model tries to return a final answer without planning.

const MAX_PLANNING_BLOCKS_BEFORE_FAILURE: usize = 5;
const MAX_PLANNING_PHASE_STEPS: usize = 10;
const MAX_PLANNING_REDIRECTS: usize = 2;
const WORKSPACE_EXTERNAL_TOOLS_PATH: &str = ".topagent/external-tools.json";
const LEGACY_WORKSPACE_COMMANDS_PATH: &str = "commands.json";

/// Number of distinct files changed without a plan before we consider
/// escalating a non-plan-required task into plan-required.
const UNPLANNED_MUTATION_ESCALATION_THRESHOLD: usize = 3;

const PLANNING_REDIRECT_MSG: &str = "\
This task requires a plan before proceeding. \
Use the update_plan tool to create a plan with concrete steps, then execute it.";
pub struct Agent {
    session: Session,
    provider: Box<dyn Provider>,
    tools: ToolRegistry,
    external_tools: ExternalToolRegistry,
    options: RuntimeOptions,
    plan: Arc<Mutex<Plan>>,
    hooks: HookRegistry,
    changed_files: RefCell<Vec<String>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashCommandClass {
    ResearchSafe,
    MutationRisk,
    Verification,
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

    pub fn with_options(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
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

        registry.add(Box::new(CreateToolTool::new()));
        registry.add(Box::new(RepairToolTool::new()));
        registry.add(Box::new(ListGeneratedToolsTool::new()));
        registry.add(Box::new(DeleteGeneratedToolTool::new()));
        registry.add(Box::new(DesignToolTool::new()));
        registry.add(Box::new(ApproveToolProposalTool::new()));
        registry.add(Box::new(RejectToolProposalTool::new()));
        registry.add(Box::new(ReviseToolProposalTool::new()));
        registry.add(Box::new(ShowToolProposalTool::new()));
        registry.add(Box::new(ImplementToolProposalTool::new()));
        registry.add(Box::new(ListToolProposalsTool::new()));

        let resolved_route = ModelRoute::default();
        Self {
            session: Session::new(),
            provider,
            tools: registry,
            external_tools: ExternalToolRegistry::new(),
            options,
            plan,
            hooks: HookRegistry::new(),
            changed_files: RefCell::new(Vec::new()),
            bash_history: RefCell::new(Vec::new()),
            planning_gate_active: false,
            planning_required_for_task: false,
            task_mode: plan::TaskMode::PlanAndExecute,
            planning_escalated: false,
            resolved_route,
            execution_stage: ExecutionStage::Research,
            external_tool_ran: RefCell::new(false),
            run_baseline: RefCell::new(None),
            progress_callback: None,
            planning_block_count: 0,
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
        if Self::is_plan_tool(name) {
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

    fn maybe_truncate_history(&mut self) {
        if self.session.message_count() > self.options.max_messages_before_truncation {
            let keep_recent = self.options.max_messages_before_truncation / 2;
            self.session.truncate_history(keep_recent);
        }
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
    fn classify_task(&self, instruction: &str) -> bool {
        match plan::heuristic_fast_path(instruction) {
            Some(result) => result,
            None => self.classify_task_with_llm(instruction),
        }
    }

    fn classify_task_with_llm(&self, instruction: &str) -> bool {
        let (system_prompt, user_msg) = plan::build_classification_messages(instruction);
        let messages = vec![Message::system(system_prompt), Message::user(user_msg)];
        let route = self.resolved_route.clone();

        match self.provider.complete(&messages, &route) {
            Ok(ProviderResponse::Message(msg)) => {
                if let Some(text) = msg.as_text() {
                    plan::parse_classification_response(text)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn classify_task_mode(&self, instruction: &str) -> plan::TaskMode {
        match plan::task_mode_fast_path(instruction) {
            Some(mode) => mode,
            None => self.classify_task_mode_with_llm(instruction),
        }
    }

    fn classify_task_mode_with_llm(&self, instruction: &str) -> plan::TaskMode {
        let (system_prompt, user_msg) = plan::build_task_mode_messages(instruction);
        let messages = vec![Message::system(system_prompt), Message::user(user_msg)];
        let route = self.resolved_route.clone();

        match self.provider.complete(&messages, &route) {
            Ok(ProviderResponse::Message(msg)) => msg
                .as_text()
                .and_then(plan::parse_task_mode_response)
                .unwrap_or(plan::TaskMode::PlanAndExecute),
            _ => plan::TaskMode::PlanAndExecute,
        }
    }

    /// Break a planning deadlock by generating a real plan via the LLM.
    /// Falls back to a minimal emergency plan if the LLM call fails.
    /// Always deactivates the planning gate afterward.
    fn generate_or_fallback_plan(&mut self, instruction: &str) {
        if self.plan_exists() {
            self.deactivate_planning_gate();
            return;
        }

        // Try a dedicated LLM plan-generation call.
        if self.try_generate_plan(instruction) {
            self.deactivate_planning_gate();
            return;
        }

        // LLM failed — create a minimal emergency plan so the agent can proceed.
        if let Ok(mut plan) = self.plan.lock() {
            plan.clear();
            plan.add_item("Execute the requested changes".to_string());
            plan.add_item("Verify the result".to_string());
        }
        self.deactivate_planning_gate();
    }

    /// Attempt to generate a concrete plan via a single LLM call.
    /// Returns true if a non-empty plan was created.
    fn try_generate_plan(&mut self, instruction: &str) -> bool {
        let prompt = plan::build_plan_generation_prompt(instruction);
        let messages = vec![Message::system(prompt.0), Message::user(prompt.1)];
        let route = self.resolved_route.clone();

        let text = match self.provider.complete(&messages, &route) {
            Ok(ProviderResponse::Message(msg)) => msg.as_text().map(|s| s.to_string()),
            _ => None,
        };

        let Some(text) = text else { return false };
        let items = plan::parse_plan_generation_response(&text);
        if items.is_empty() {
            return false;
        }

        if let Ok(mut plan) = self.plan.lock() {
            plan.clear();
            for item in items {
                plan.add_item(item);
            }
        }
        true
    }

    fn note_planning_block(&mut self, instruction: &str) -> Result<()> {
        if !self.planning_gate_active || self.plan_exists() {
            self.planning_block_count = 0;
            return Ok(());
        }

        self.planning_block_count += 1;
        if self.planning_block_count >= MAX_PLANNING_BLOCKS_BEFORE_FAILURE {
            self.generate_or_fallback_plan(instruction);
        }

        Ok(())
    }

    /// Check whether a task that was *not* initially classified as
    /// plan-required should be escalated based on runtime mutation signals.
    /// Activates the planning gate if multiple distinct files have been
    /// changed without any plan in place.
    fn maybe_escalate_to_planning(&mut self) {
        // Only escalate once; don't re-escalate after auto-plan resolved it.
        if self.planning_gate_active || self.planning_escalated || self.plan_exists() {
            return;
        }
        if !self.options.require_plan {
            return;
        }
        let distinct_files = self.changed_files.borrow().len();
        if distinct_files >= UNPLANNED_MUTATION_ESCALATION_THRESHOLD {
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
    ) -> Option<PreflightBlock> {
        let tool_ctx = ToolContext::new(ctx, &self.options);
        if let Some(hooks) = self.hooks.get(name) {
            if !hooks.run_pre_hooks(name, args, &tool_ctx) {
                return Some(PreflightBlock {
                    message: "error: tool blocked by pre-hook".into(),
                    is_planning_block: false,
                });
            }
        }

        if let Some(block_msg) = self.check_planning_gate(name, bash_args, external_effect) {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Some(PreflightBlock {
                message: block_msg,
                is_planning_block: true,
            });
        }
        if let Some(block_msg) =
            self.check_pre_execution_verification_gate(name, bash_args, external_effect)
        {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Some(PreflightBlock {
                message: block_msg,
                is_planning_block: false,
            });
        }

        None
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
        let bash_args = if name == "bash" { Some(&args) } else { None };
        if let Some(block) = self.run_preflight(ctx, &name, &args, bash_args, None) {
            self.record_tool_result(id, name, args, block.message);
            if block.is_planning_block {
                self.note_planning_block(instruction)?;
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

        if Self::is_mutation_tool(&name) {
            self.reconcile_changed_files(&ctx.workspace_root);
            self.mark_execution_started();
            self.maybe_escalate_to_planning();
        }

        if Self::is_plan_tool(&name) && self.plan_exists() {
            self.deactivate_planning_gate();
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

        // ── Preflight: hooks + gates ──
        if let Some(block) = self.run_preflight(ctx, &name, &args, None, Some(external_effect)) {
            self.record_tool_result(id, name, args, block.message);
            if block.is_planning_block {
                self.note_planning_block(instruction)?;
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

    fn record_changed_file(&self, path: String) {
        if self.is_pre_existing_dirty(&path) {
            return;
        }
        let mut changed = self.changed_files.borrow_mut();
        if !changed.contains(&path) {
            changed.push(path);
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

    fn task_requires_concrete_execution(&self) -> bool {
        matches!(self.task_mode, plan::TaskMode::PlanAndExecute)
    }

    fn should_block_pre_execution_actions(&self) -> bool {
        self.planning_required_for_task
            && self.plan_exists()
            && !self.execution_started()
            && self.task_requires_concrete_execution()
    }

    fn check_pre_execution_verification_gate(
        &self,
        tool_name: &str,
        bash_args: Option<&serde_json::Value>,
        external_effect: Option<ExternalToolEffect>,
    ) -> Option<String> {
        if !self.should_block_pre_execution_actions() {
            return None;
        }

        if tool_name == "bash" {
            let args = bash_args?;
            let cmd = args.get("command").and_then(|c| c.as_str())?;
            if Self::classify_bash_command(cmd) == BashCommandClass::Verification {
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

    fn is_mutation_tool(name: &str) -> bool {
        matches!(name, "write" | "edit" | "git_commit" | "git_add")
    }

    fn is_plan_tool(name: &str) -> bool {
        matches!(name, "update_plan" | "save_plan")
    }

    pub fn classify_bash_command(cmd: &str) -> BashCommandClass {
        let trimmed = cmd.trim();
        let lower = trimmed.to_lowercase();

        if Self::is_verification_command(trimmed) {
            return BashCommandClass::Verification;
        }

        // Detect common mutation patterns before safe prefixes
        if lower.contains(" >")
            || lower.contains(">>")
            || lower.contains("|")
            || lower.contains("rm ")
            || lower.contains("mv ")
            || lower.contains("cp ")
            || lower.contains("touch ")
            || lower.contains("mkdir ")
            || lower.contains("echo ") && lower.contains(">")
        {
            return BashCommandClass::MutationRisk;
        }

        let research_safe_prefixes = [
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
        ];

        for prefix in research_safe_prefixes {
            if lower.starts_with(prefix) || lower == prefix.trim_end_matches(' ') {
                return BashCommandClass::ResearchSafe;
            }
        }

        BashCommandClass::MutationRisk
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
        if Self::is_plan_tool(tool_name) {
            return None;
        }

        if tool_name == "bash" {
            let plan_exists = self.plan.lock().map(|p| !p.is_empty()).unwrap_or(false);
            if plan_exists {
                return None;
            }
            if let Some(args) = bash_args {
                if let Some(cmd) = args.get("command").and_then(|c| c.as_str()) {
                    let class = Self::classify_bash_command(cmd);
                    if class == BashCommandClass::ResearchSafe {
                        return None;
                    }
                }
            }
            if bash_args.is_none() {
                return Some(
                    "Planning required for this task. Please create a plan using update_plan before running bash commands.".to_string(),
                );
            }
            return Some(
                "Planning required for this task. Use update_plan to create a plan before mutation commands.".to_string(),
            );
        }

        if let Some(effect) = external_effect {
            if self.plan_exists() {
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

        if !Self::is_mutation_tool(tool_name) {
            return None;
        }
        if let Ok(plan) = self.plan.lock() {
            if !plan.is_empty() {
                return None;
            }
        }
        Some(format!(
            "Planning required for this task. Please create a plan using update_plan before using {}.",
            tool_name
        ))
    }

    fn is_verification_command(cmd: &str) -> bool {
        let lower = cmd.to_lowercase();

        if lower.starts_with("cargo test")
            || lower.starts_with("cargo build")
            || lower.starts_with("cargo check")
            || lower.starts_with("cargo clippy")
            || lower.starts_with("cargo fmt")
            || lower.starts_with("cargo watch")
            || lower.starts_with("cargo auditable")
            || lower.starts_with("pytest")
            || lower.starts_with("py.test")
            || lower.starts_with("make test")
            || lower.starts_with("make check")
            || lower.starts_with("make verify")
            || lower.starts_with("npm test")
            || lower.starts_with("npm run test")
            || lower.starts_with("npm run build")
            || lower.starts_with("npm run check")
            || lower.starts_with("go test")
            || lower.starts_with("go build")
            || lower.starts_with("go vet")
            || lower.starts_with("rustfmt")
            || lower.starts_with("rust-analyzer")
            || lower.starts_with("clippy")
            || lower.starts_with("deny ")
            || lower.starts_with("audit ")
            || lower.starts_with("cargo deny")
            || lower.starts_with("cargo audit")
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
            let verification_indicators = ["test", "build", "check", "lint", "fmt", "audit", "vet"];
            for indicator in verification_indicators {
                if lower.contains(indicator) {
                    return true;
                }
            }
        }

        false
    }

    fn extract_bash_command(args: &serde_json::Value) -> String {
        args.get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    pub fn load_external_tools_from_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let content = std::fs::read_to_string(path).map_err(Error::Io)?;
        self.external_tools.load_from_str(&content)
    }

    pub fn load_workspace_external_tools(&mut self, workspace_root: &Path) -> Result<()> {
        for path in Self::workspace_external_tool_paths(workspace_root) {
            if !path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&path).map_err(Error::Io)?;
            self.external_tools.load_from_str(&content)?;
        }
        Ok(())
    }

    pub fn load_generated_tools_from_workspace(&mut self, workspace_root: &Path) -> Result<()> {
        let genesis = ToolGenesis::new(workspace_root.to_path_buf());
        let verified_tools = genesis.load_verified_tools()?;
        for tool in verified_tools {
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
        self.reload_workspace_tools(&ctx.workspace_root)?;
        self.reset_run_state(&ctx.workspace_root, instruction);
        self.emit_progress(self.current_working_progress());

        self.session.add_message(Message::user(instruction));
        self.session
            .set_system_prompt(&self.build_run_system_prompt(ctx)?);

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

            // Planning phase budget: if the model spent too many steps
            // researching without creating a plan, generate one.
            if self.planning_gate_active && !self.plan_exists() {
                planning_phase_steps += 1;
                if planning_phase_steps >= MAX_PLANNING_PHASE_STEPS {
                    self.generate_or_fallback_plan(instruction);
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
                            if planning_redirects >= MAX_PLANNING_REDIRECTS {
                                // Model repeatedly refused to plan — generate one.
                                self.generate_or_fallback_plan(instruction);
                                self.emit_progress(self.current_working_progress());
                            }
                            self.redirect_to_planning(msg, PLANNING_REDIRECT_MSG);
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
                    self.maybe_truncate_history();
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
                    self.maybe_truncate_history();
                }
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
    }

    fn workspace_external_tool_paths(workspace_root: &Path) -> [std::path::PathBuf; 2] {
        [
            workspace_root.join(LEGACY_WORKSPACE_COMMANDS_PATH),
            workspace_root.join(WORKSPACE_EXTERNAL_TOOLS_PATH),
        ]
    }

    fn reload_workspace_tools(&mut self, workspace_root: &Path) -> Result<()> {
        self.external_tools = ExternalToolRegistry::new();
        self.load_workspace_external_tools(workspace_root)?;
        self.load_generated_tools_from_workspace(workspace_root)?;
        Ok(())
    }

    fn reset_run_state(&mut self, workspace_root: &Path, instruction: &str) {
        self.changed_files.borrow_mut().clear();
        self.bash_history.borrow_mut().clear();
        *self.external_tool_ran.borrow_mut() = false;
        self.capture_run_baseline(workspace_root);

        self.planning_required_for_task =
            self.options.require_plan && self.classify_task(instruction);
        self.task_mode = if self.planning_required_for_task {
            self.classify_task_mode(instruction)
        } else {
            plan::TaskMode::PlanAndExecute
        };
        self.planning_gate_active = self.planning_required_for_task;
        self.planning_escalated = false;
        self.planning_block_count = 0;
        self.execution_stage = ExecutionStage::Research;
    }

    fn build_run_system_prompt(&self, ctx: &ExecutionContext) -> Result<String> {
        let mut system_prompt =
            prompt::build_system_prompt(&self.tools.specs(), &self.external_tools.specs());

        match get_project_instructions_or_error(&ctx.workspace_root)? {
            Some(project_instructions) => {
                system_prompt.push_str("\n## Project Instructions (from TOPAGENT.md)\n\n");
                system_prompt.push_str(&project_instructions);
                system_prompt.push('\n');
            }
            None => system_prompt.push_str(prompt::NO_PI_MD_NOTE),
        }

        if let Some(memory_context) = ctx.memory_context() {
            system_prompt.push_str("\n## Workspace Memory\n\n");
            system_prompt.push_str(memory_context);
            system_prompt.push('\n');
        }

        if let Ok(plan) = self.plan.lock() {
            if !plan.is_empty() {
                system_prompt.push_str("\n## Current Plan\n\n");
                system_prompt.push_str(&plan.format_for_display());
            }
        }

        if self.planning_gate_active && !self.plan_exists() {
            system_prompt.push_str(
                "\n## Planning Required\n\n\
                This task is non-trivial. Before making changes:\n\
                1. Research: inspect relevant files and git context\n\
                2. Plan: use update_plan to create a plan with clear steps\n\
                3. Build: execute plan items, updating status as you complete each step\n\n",
            );
        }

        Ok(system_prompt)
    }

    fn build_proof_of_work(&self, response: &str, workspace_root: &Path) -> String {
        let files = self.changed_files.borrow().clone();
        let unattributed_files = self.unattributed_pre_existing_dirty_files(workspace_root);
        if files.is_empty()
            && self.bash_history.borrow().is_empty()
            && unattributed_files.is_empty()
        {
            return response.to_string();
        }

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
        };

        for (command, full_output, exit_code) in self.bash_history.borrow().iter() {
            if Self::is_verification_command(command) {
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

        let task_result = TaskResult::new(response.to_string())
            .with_files_changed(evidence.files_changed.clone())
            .with_diff_summary(evidence.diff_summary.clone())
            .with_verification_commands(evidence.verification_commands_run.clone())
            .with_unresolved_issues(evidence.unresolved_issues.clone());

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
    use crate::context::ExecutionContext;
    use crate::provider::{ProviderResponse, ScriptedProvider};
    use crate::runtime::RuntimeOptions;
    use crate::tools::default_tools;
    use crate::Message;
    use std::fs;
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
}

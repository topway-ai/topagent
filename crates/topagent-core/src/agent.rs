use crate::context::{ExecutionContext, ToolContext};
use crate::external::ExternalToolRegistry;
use crate::hooks::HookRegistry;
use crate::model::{ModelRoute, RoutingPolicy};
use crate::plan::{should_require_research_plan_build, Plan};
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

const MAX_PLANNING_BLOCKS_BEFORE_FAILURE: usize = 5;

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

        let resolved_route = RoutingPolicy::select_route(options.task_category, None);
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
                    ProgressUpdate::working(format!("Running tool: bash (verification)"))
                }
                _ => ProgressUpdate::running_tool("bash"),
            };
        }

        match name {
            "write" | "edit" | "git_add" | "git_commit" => ProgressUpdate::running_tool(name),
            _ => ProgressUpdate::running_tool(name),
        }
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

    fn planning_deadlock_error() -> Error {
        Error::Session(
            "planning is required for this task, but no plan could be created; task is blocked"
                .to_string(),
        )
    }

    fn note_planning_block(&mut self) -> Result<()> {
        if !self.planning_gate_active || self.plan_exists() {
            self.planning_block_count = 0;
            return Ok(());
        }

        self.planning_block_count += 1;
        if self.planning_block_count >= MAX_PLANNING_BLOCKS_BEFORE_FAILURE {
            return Err(Self::planning_deadlock_error());
        }

        Ok(())
    }

    fn clear_planning_block_state(&mut self) {
        self.planning_block_count = 0;
    }

    fn planning_still_blocked(&self) -> bool {
        self.planning_gate_active && !self.plan_exists() && self.planning_block_count > 0
    }

    fn check_cancelled(&self, ctx: &ExecutionContext) -> Result<()> {
        if ctx.is_cancelled() {
            return Err(Self::stop_error());
        }
        Ok(())
    }

    fn track_changed_file(&self, tool_name: &str, args: &serde_json::Value) {
        if tool_name == "write" || tool_name == "edit" {
            if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                self.record_changed_file(path.to_string());
            }
        }
    }

    fn extract_changed_path(&self, tool_name: &str, args: &serde_json::Value) -> Option<String> {
        if tool_name == "write" || tool_name == "edit" {
            args.get("path")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
        } else {
            None
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
                    if class == BashCommandClass::ResearchSafe
                        || class == BashCommandClass::Verification
                    {
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

    pub fn load_commands_from_workspace(&mut self, workspace_root: &Path) -> Result<()> {
        let commands_path = workspace_root.join("commands.json");
        if !commands_path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&commands_path).map_err(Error::Io)?;
        self.external_tools.load_from_str(&content)?;
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
        self.external_tools = ExternalToolRegistry::new();
        self.load_commands_from_workspace(&ctx.workspace_root)?;
        self.load_generated_tools_from_workspace(&ctx.workspace_root)?;

        self.changed_files.borrow_mut().clear();
        self.bash_history.borrow_mut().clear();
        *self.external_tool_ran.borrow_mut() = false;

        self.capture_run_baseline(&ctx.workspace_root);

        self.planning_gate_active =
            self.options.require_plan && should_require_research_plan_build(instruction);
        self.planning_block_count = 0;

        self.execution_stage = ExecutionStage::Research;
        self.emit_progress(self.current_working_progress());

        self.session.add_message(Message::user(instruction));

        let mut system_prompt =
            prompt::build_system_prompt(&self.tools.specs(), &self.external_tools.specs());

        match get_project_instructions_or_error(&ctx.workspace_root)? {
            Some(project_instructions) => {
                system_prompt.push_str("\n## Project Instructions (from TOPAGENT.md)\n\n");
                system_prompt.push_str(&project_instructions);
                system_prompt.push('\n');
            }
            None => {
                system_prompt.push_str(prompt::NO_PI_MD_NOTE);
            }
        }

        if let Ok(plan) = self.plan.lock() {
            if !plan.is_empty() {
                system_prompt.push_str("\n## Current Plan\n\n");
                system_prompt.push_str(&plan.format_for_display());
            }
        }

        if self.options.require_plan && should_require_research_plan_build(instruction) {
            if let Ok(plan) = self.plan.lock() {
                if plan.is_empty() {
                    system_prompt.push_str(
                        "\n## Planning Required\n\n\
                        This task is non-trivial. Before making changes:\n\
                        1. Research: inspect relevant files and git context\n\
                        2. Plan: use update_plan to create a plan with clear steps\n\
                        3. Build: execute plan items, updating status as you complete each step\n\n",
                    );
                }
            }
        }

        self.session.set_system_prompt(&system_prompt);

        let mut steps = 0;
        let mut empty_response_retries = 0;

        loop {
            self.check_cancelled(ctx)?;
            if steps >= self.options.max_steps {
                return Err(Error::MaxStepsReached(format!(
                    "max steps ({}) reached without completing task",
                    self.options.max_steps
                )));
            }

            self.emit_progress(ProgressUpdate::waiting_for_model(
                self.current_progress_phase(),
            ));
            let response = match self.provider.complete_with_cancel(
                &self.session.messages(),
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
                        if self.planning_still_blocked() {
                            return Err(Self::planning_deadlock_error());
                        }
                        self.session.add_message(msg);
                        let final_response = self.build_proof_of_work(&text, &ctx.workspace_root);
                        return Ok(final_response);
                    }
                    self.session.add_message(msg);
                }
                ProviderResponse::ToolCall { id, name, args } => {
                    let tool_ctx = ToolContext::new(ctx, &self.options);
                    let is_external = self.external_tools.get(&name).is_some();
                    let tool = match self.tools.get(&name) {
                        Some(t) => t,
                        None if is_external => {
                            let external_tool = self.external_tools.get(&name).unwrap();
                            if let Some(hooks) = self.hooks.get(&name) {
                                if !hooks.run_pre_hooks(&name, &args, &tool_ctx) {
                                    self.session.add_message(Message::tool_request(
                                        id.clone(),
                                        name.clone(),
                                        args,
                                    ));
                                    self.session.add_message(Message::tool_result(
                                        id,
                                        format!(
                                            "error: external tool '{}' blocked by pre-hook",
                                            name
                                        ),
                                    ));
                                    continue;
                                }
                            }
                            if let Some(block_msg) = self.check_planning_gate(&name, None) {
                                self.emit_progress(Self::blocked_progress(&block_msg));
                                self.session.add_message(Message::tool_request(
                                    id.clone(),
                                    name.clone(),
                                    args,
                                ));
                                self.session
                                    .add_message(Message::tool_result(id, block_msg));
                                self.note_planning_block()?;
                                continue;
                            }
                            self.emit_progress(self.tool_progress(&name, &args));
                            self.check_cancelled(ctx)?;
                            let result = external_tool.execute(&args, &tool_ctx);
                            self.check_cancelled(ctx)?;
                            *self.external_tool_ran.borrow_mut() = true;
                            let found_new_change =
                                self.reconcile_changed_files(&ctx.workspace_root);
                            if found_new_change && self.execution_stage == ExecutionStage::Research
                            {
                                self.execution_stage = ExecutionStage::Edit;
                            }
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args,
                            ));
                            let result_str = match result {
                                Ok(r) => r,
                                Err(e) => {
                                    self.session.add_message(Message::tool_result(
                                        id,
                                        format!("error: external tool execution failed: {}", e),
                                    ));
                                    empty_response_retries = 0;
                                    if self.session.message_count()
                                        > self.options.max_messages_before_truncation
                                    {
                                        let keep_recent =
                                            self.options.max_messages_before_truncation / 2;
                                        self.session.truncate_history(keep_recent);
                                    }
                                    continue;
                                }
                            };
                            self.session
                                .add_message(Message::tool_result(id, result_str));
                            empty_response_retries = 0;
                            if self.session.message_count()
                                > self.options.max_messages_before_truncation
                            {
                                let keep_recent = self.options.max_messages_before_truncation / 2;
                                self.session.truncate_history(keep_recent);
                            }
                            continue;
                        }
                        None => {
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args,
                            ));
                            self.session.add_message(Message::tool_result(
                                id,
                                format!("error: unknown tool '{}'", name),
                            ));
                            continue;
                        }
                    };

                    if let Some(hooks) = self.hooks.get(&name) {
                        if !hooks.run_pre_hooks(&name, &args, &tool_ctx) {
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args,
                            ));
                            self.session.add_message(Message::tool_result(
                                id,
                                format!("error: tool '{}' blocked by pre-hook", name),
                            ));
                            continue;
                        }
                    }

                    let bash_args = if name == "bash" { Some(&args) } else { None };
                    if let Some(block_msg) = self.check_planning_gate(&name, bash_args) {
                        self.emit_progress(Self::blocked_progress(&block_msg));
                        self.session.add_message(Message::tool_request(
                            id.clone(),
                            name.clone(),
                            args,
                        ));
                        self.session
                            .add_message(Message::tool_result(id, block_msg));
                        self.note_planning_block()?;
                        continue;
                    }

                    let changed_path = self.extract_changed_path(&name, &args);
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
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args,
                            ));
                            self.session.add_message(Message::tool_result(
                                id,
                                format!("error: tool execution failed: {}", e),
                            ));
                            continue;
                        }
                    };
                    self.check_cancelled(ctx)?;

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
                            let found_new_change =
                                self.reconcile_changed_files(&ctx.workspace_root);
                            if found_new_change && self.execution_stage == ExecutionStage::Research
                            {
                                self.execution_stage = ExecutionStage::Edit;
                            }
                        }
                    }

                    if let Some(hooks) = self.hooks.get(&name) {
                        result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
                    }

                    if let Some(path) = changed_path {
                        self.record_changed_file(path);
                    }

                    if Self::is_mutation_tool(&name) {
                        self.reconcile_changed_files(&ctx.workspace_root);
                        if self.execution_stage == ExecutionStage::Research {
                            self.execution_stage = ExecutionStage::Edit;
                        }
                    }

                    if Self::is_plan_tool(&name) {
                        if self.plan_exists() {
                            self.planning_gate_active = false;
                            self.clear_planning_block_state();
                        }
                    }

                    self.session
                        .add_message(Message::tool_request(id.clone(), name, args));
                    self.session.add_message(Message::tool_result(id, result));
                    empty_response_retries = 0;

                    if self.session.message_count() > self.options.max_messages_before_truncation {
                        let keep_recent = self.options.max_messages_before_truncation / 2;
                        self.session.truncate_history(keep_recent);
                    }
                }
                ProviderResponse::ToolCalls(calls) => {
                    for call in calls {
                        let tool_ctx = ToolContext::new(ctx, &self.options);
                        let id = call.id;
                        let name = call.name;
                        let args = call.args;
                        let is_external = self.external_tools.get(&name).is_some();
                        let tool = match self.tools.get(&name) {
                            Some(t) => t,
                            None if is_external => {
                                let external_tool = self.external_tools.get(&name).unwrap();
                                if let Some(hooks) = self.hooks.get(&name) {
                                    if !hooks.run_pre_hooks(&name, &args, &tool_ctx) {
                                        self.session.add_message(Message::tool_request(
                                            id.clone(),
                                            name.clone(),
                                            args.clone(),
                                        ));
                                        self.session.add_message(Message::tool_result(
                                            id,
                                            format!(
                                                "error: external tool '{}' blocked by pre-hook",
                                                name
                                            ),
                                        ));
                                        continue;
                                    }
                                }
                                if let Some(block_msg) = self.check_planning_gate(&name, None) {
                                    self.emit_progress(Self::blocked_progress(&block_msg));
                                    self.session.add_message(Message::tool_request(
                                        id.clone(),
                                        name.clone(),
                                        args.clone(),
                                    ));
                                    self.session
                                        .add_message(Message::tool_result(id, block_msg));
                                    self.note_planning_block()?;
                                    continue;
                                }
                                self.emit_progress(self.tool_progress(&name, &args));
                                self.check_cancelled(ctx)?;
                                let result = external_tool.execute(&args, &tool_ctx);
                                self.check_cancelled(ctx)?;
                                *self.external_tool_ran.borrow_mut() = true;
                                let found_new_change =
                                    self.reconcile_changed_files(&ctx.workspace_root);
                                if found_new_change
                                    && self.execution_stage == ExecutionStage::Research
                                {
                                    self.execution_stage = ExecutionStage::Edit;
                                }
                                self.session.add_message(Message::tool_request(
                                    id.clone(),
                                    name.clone(),
                                    args.clone(),
                                ));
                                let result_str = match result {
                                    Ok(r) => r,
                                    Err(e) => {
                                        self.session.add_message(Message::tool_result(
                                            id,
                                            format!("error: external tool execution failed: {}", e),
                                        ));
                                        continue;
                                    }
                                };
                                self.session
                                    .add_message(Message::tool_result(id, result_str));
                                continue;
                            }
                            None => {
                                self.session.add_message(Message::tool_request(
                                    id.clone(),
                                    name.clone(),
                                    args.clone(),
                                ));
                                self.session.add_message(Message::tool_result(
                                    id,
                                    format!("error: unknown tool '{}'", name),
                                ));
                                continue;
                            }
                        };

                        if let Some(hooks) = self.hooks.get(&name) {
                            if !hooks.run_pre_hooks(&name, &args, &tool_ctx) {
                                self.session.add_message(Message::tool_request(
                                    id.clone(),
                                    name.clone(),
                                    args.clone(),
                                ));
                                self.session.add_message(Message::tool_result(
                                    id,
                                    format!("error: tool '{}' blocked by pre-hook", name),
                                ));
                                continue;
                            }
                        }

                        let bash_args = if name == "bash" { Some(&args) } else { None };
                        if let Some(block_msg) = self.check_planning_gate(&name, bash_args) {
                            self.emit_progress(Self::blocked_progress(&block_msg));
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args.clone(),
                            ));
                            self.session
                                .add_message(Message::tool_result(id, block_msg));
                            self.note_planning_block()?;
                            continue;
                        }

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
                                self.session.add_message(Message::tool_request(
                                    id.clone(),
                                    name.clone(),
                                    args.clone(),
                                ));
                                self.session.add_message(Message::tool_result(
                                    id,
                                    format!("error: tool execution failed: {}", e),
                                ));
                                continue;
                            }
                        };
                        self.check_cancelled(ctx)?;

                        if name == "bash" {
                            let class = if let Some(cmd_str) = &bash_cmd {
                                Self::classify_bash_command(cmd_str)
                            } else {
                                BashCommandClass::MutationRisk
                            };
                            if let Some(cmd) = bash_cmd {
                                let exit_code = extract_exit_code(&result);
                                self.bash_history.borrow_mut().push((
                                    cmd,
                                    result.clone(),
                                    exit_code,
                                ));
                            }
                            if matches!(
                                class,
                                BashCommandClass::MutationRisk | BashCommandClass::Verification
                            ) {
                                let found_new_change =
                                    self.reconcile_changed_files(&ctx.workspace_root);
                                if found_new_change
                                    && self.execution_stage == ExecutionStage::Research
                                {
                                    self.execution_stage = ExecutionStage::Edit;
                                }
                            }
                        }

                        if let Some(hooks) = self.hooks.get(&name) {
                            result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
                        }

                        self.track_changed_file(&name, &args);

                        if Self::is_mutation_tool(&name) {
                            self.reconcile_changed_files(&ctx.workspace_root);
                            if self.execution_stage == ExecutionStage::Research {
                                self.execution_stage = ExecutionStage::Edit;
                            }
                        }

                        if Self::is_plan_tool(&name) {
                            if self.plan_exists() {
                                self.planning_gate_active = false;
                                self.clear_planning_block_state();
                            }
                        }

                        self.session
                            .add_message(Message::tool_request(id.clone(), name, args));
                        self.session.add_message(Message::tool_result(id, result));
                    }
                    empty_response_retries = 0;
                    if self.session.message_count() > self.options.max_messages_before_truncation {
                        let keep_recent = self.options.max_messages_before_truncation / 2;
                        self.session.truncate_history(keep_recent);
                    }
                }
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
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
        0
    }
}

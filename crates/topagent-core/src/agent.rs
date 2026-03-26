use crate::context::{ExecutionContext, ToolContext};
use crate::external::ExternalToolRegistry;
use crate::hooks::HookRegistry;
use crate::model::{ModelRoute, RoutingPolicy};
use crate::plan::{should_require_research_plan_build, Plan};
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
use std::path::Path;
use std::sync::{Arc, Mutex};

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

    fn track_changed_file(&self, tool_name: &str, args: &serde_json::Value) {
        if tool_name == "write" || tool_name == "edit" {
            if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                let mut changed = self.changed_files.borrow_mut();
                if !changed.contains(&path.to_string()) {
                    changed.push(path.to_string());
                }
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
        let mut changed = self.changed_files.borrow_mut();
        if !changed.contains(&path) {
            changed.push(path);
        }
    }

    pub fn get_route(&self) -> ModelRoute {
        self.resolved_route.clone()
    }

    fn is_mutation_tool(name: &str) -> bool {
        matches!(name, "write" | "edit" | "bash" | "git_commit" | "git_add")
    }

    fn is_plan_tool(name: &str) -> bool {
        matches!(name, "update_plan" | "save_plan")
    }

    fn check_planning_gate(&self, tool_name: &str) -> Option<String> {
        if !self.planning_gate_active {
            return None;
        }
        if Self::is_plan_tool(tool_name) {
            return None;
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
        lower.contains("test")
            || lower.contains("build")
            || lower.contains("check")
            || lower.contains("verify")
            || lower.contains("lint")
            || lower.contains("clippy")
            || lower.contains("fmt")
            || lower.contains("format")
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
        self.external_tools = ExternalToolRegistry::new();
        self.load_commands_from_workspace(&ctx.workspace_root)?;
        self.load_generated_tools_from_workspace(&ctx.workspace_root)?;

        self.planning_gate_active =
            self.options.require_plan && should_require_research_plan_build(instruction);

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

        let tool_ctx = ToolContext::new(ctx, &self.options);
        let mut steps = 0;
        let mut empty_response_retries = 0;

        loop {
            if steps >= self.options.max_steps {
                return Err(Error::MaxStepsReached(format!(
                    "max steps ({}) reached without completing task",
                    self.options.max_steps
                )));
            }

            let response = match self.provider.complete(&self.session.messages()) {
                Ok(r) => r,
                Err(e) => {
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
                            continue;
                        }
                        self.session.add_message(msg);
                        let final_response = self.build_proof_of_work(&text);
                        return Ok(final_response);
                    }
                    self.session.add_message(msg);
                }
                ProviderResponse::ToolCall { id, name, args } => {
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
                            let result = external_tool.execute(&args, &tool_ctx);
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

                    if let Some(block_msg) = self.check_planning_gate(&name) {
                        self.session.add_message(Message::tool_request(
                            id.clone(),
                            name.clone(),
                            args,
                        ));
                        self.session
                            .add_message(Message::tool_result(id, block_msg));
                        continue;
                    }

                    let changed_path = self.extract_changed_path(&name, &args);
                    let bash_cmd = if name == "bash" {
                        Some(Self::extract_bash_command(&args))
                    } else {
                        None
                    };
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

                    if name == "bash" {
                        if let Some(cmd) = bash_cmd {
                            let exit_code = extract_exit_code(&result);
                            self.bash_history
                                .borrow_mut()
                                .push((cmd, result.clone(), exit_code));
                        }
                    }

                    if let Some(hooks) = self.hooks.get(&name) {
                        result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
                    }

                    if let Some(path) = changed_path {
                        self.record_changed_file(path);
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
                                let result = external_tool.execute(&args, &tool_ctx);
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

                        if let Some(block_msg) = self.check_planning_gate(&name) {
                            self.session.add_message(Message::tool_request(
                                id.clone(),
                                name.clone(),
                                args.clone(),
                            ));
                            self.session
                                .add_message(Message::tool_result(id, block_msg));
                            continue;
                        }

                        let bash_cmd = if name == "bash" {
                            Some(Self::extract_bash_command(&args))
                        } else {
                            None
                        };
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

                        if name == "bash" {
                            if let Some(cmd) = bash_cmd {
                                let exit_code = extract_exit_code(&result);
                                self.bash_history.borrow_mut().push((
                                    cmd,
                                    result.clone(),
                                    exit_code,
                                ));
                            }
                        }

                        if let Some(hooks) = self.hooks.get(&name) {
                            result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
                        }

                        self.track_changed_file(&name, &args);
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

    fn build_proof_of_work(&self, response: &str) -> String {
        let files = self.changed_files.borrow().clone();
        if files.is_empty() && self.bash_history.borrow().is_empty() {
            return response.to_string();
        }

        let mut evidence = TaskEvidence {
            files_changed: files,
            diff_summary: String::new(),
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

        let task_result = TaskResult::new(response.to_string())
            .with_files_changed(evidence.files_changed.clone())
            .with_verification_commands(evidence.verification_commands_run.clone());

        task_result.format_proof_of_work()
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

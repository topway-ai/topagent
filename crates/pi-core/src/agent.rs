use crate::context::{ExecutionContext, ToolContext};
use crate::external::ExternalToolRegistry;
use crate::hooks::HookRegistry;
use crate::plan::Plan;
use crate::project::get_project_instructions_or_error;
use crate::prompt;
use crate::runtime::RuntimeOptions;
use crate::session::Session;
use crate::tools::{Tool, ToolRegistry, UpdatePlanTool};
use crate::{Error, Message, Provider, ProviderResponse, Result};
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

        Self {
            session: Session::new(),
            provider,
            tools: registry,
            external_tools: ExternalToolRegistry::new(),
            options,
            plan,
            hooks: HookRegistry::new(),
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

    pub fn external_tools(&self) -> &ExternalToolRegistry {
        &self.external_tools
    }

    pub fn external_tools_mut(&mut self) -> &mut ExternalToolRegistry {
        &mut self.external_tools
    }

    pub fn load_external_tools_from_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let content = std::fs::read_to_string(path).map_err(Error::Io)?;
        self.external_tools.load_from_str(&content)
    }

    pub fn run(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.session.add_message(Message::user(instruction));

        let mut system_prompt =
            prompt::build_system_prompt(&self.tools.specs(), &self.external_tools.specs());

        if let Ok(Some(project_instructions)) =
            get_project_instructions_or_error(&ctx.workspace_root)
        {
            system_prompt.push_str("\n## Project Instructions (from PI.md)\n\n");
            system_prompt.push_str(&project_instructions);
            system_prompt.push('\n');
        }

        if let Ok(plan) = self.plan.lock() {
            if !plan.is_empty() {
                system_prompt.push_str("\n## Current Plan\n\n");
                system_prompt.push_str(&plan.format_for_display());
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
                        return Ok(text);
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

                    if let Some(hooks) = self.hooks.get(&name) {
                        result = hooks.run_post_hooks(&name, &args, &result, &tool_ctx);
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
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
    }
}

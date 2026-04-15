use super::Agent;
use crate::context::ExecutionContext;
use crate::hooks::{dispatch_hooks, HookEvent, HookInput};
use crate::task_result::VerificationCommand;
use crate::{Error, Message, ProviderResponse, Result};
use std::process::Command;

#[derive(Default)]
struct LoopCounters {
    steps: usize,
    empty_response_retries: usize,
    planning_phase_steps: usize,
    planning_redirects: usize,
}

impl Agent {
    pub fn run(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.emit_progress(crate::progress::ProgressUpdate::received());

        let result = self.run_inner(ctx, instruction);
        match &result {
            Ok(_) => self.emit_progress(crate::progress::ProgressUpdate::completed()),
            Err(Error::Stopped(_)) => {
                self.emit_progress(crate::progress::ProgressUpdate::stopped())
            }
            Err(err) => {
                self.emit_progress(crate::progress::ProgressUpdate::failed(err.to_string()))
            }
        }
        result
    }

    fn run_inner(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.check_cancelled(ctx)?;
        self.reset_run_state(ctx, instruction)?;
        self.reload_workspace_tools(&ctx.workspace_root)?;
        self.emit_progress(self.current_working_progress());

        // OnSessionStart hooks: inject bounded context before the step loop
        self.run_on_session_start_hooks(ctx, instruction);

        self.session.add_message(Message::user(instruction));

        let mut counters = LoopCounters::default();
        let mut provider_msgs = Vec::new();

        loop {
            self.check_cancelled(ctx)?;
            if counters.steps >= self.options.max_steps {
                return Err(Error::MaxStepsReached(format!(
                    "max steps ({}) reached without completing task",
                    self.options.max_steps
                )));
            }

            self.maybe_compact_context(ctx);
            if self.behavior.compaction.refresh_system_prompt_each_turn || counters.steps == 0 {
                self.session
                    .set_system_prompt(&self.build_run_system_prompt(ctx)?);
            }

            if self.planning.is_active() && !self.plan_exists() {
                counters.planning_phase_steps += 1;
                if counters.planning_phase_steps
                    >= self.behavior.planning.max_research_steps_without_plan
                {
                    self.generate_or_fallback_plan(instruction, ctx.cancel_token())?;
                    self.emit_progress(self.current_working_progress());
                }
            }

            self.emit_progress(crate::progress::ProgressUpdate::waiting_for_model(
                self.current_progress_phase(),
            ));
            self.session.fill_messages(&mut provider_msgs);
            let response = match self.provider.complete_with_cancel(
                &provider_msgs,
                &self.resolved_route,
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
                    if counters.empty_response_retries >= self.options.max_provider_retries {
                        return Err(Error::ProviderRetryExhausted(format!(
                            "provider failed after {} retries: {}",
                            self.options.max_provider_retries, e
                        )));
                    }
                    counters.empty_response_retries += 1;
                    if counters.empty_response_retries >= self.options.max_provider_retries {
                        return Err(Error::ProviderRetryExhausted(format!(
                            "provider failed repeatedly ({} attempts): {}",
                            counters.empty_response_retries, e
                        )));
                    }
                    self.emit_progress(crate::progress::ProgressUpdate::retrying_provider(
                        counters.empty_response_retries,
                        self.options.max_provider_retries,
                    ));
                    continue;
                }
            };

            counters.steps += 1;

            match response {
                ProviderResponse::Message(msg) => {
                    let text = msg.as_text().map(|s| s.to_string());
                    if let Some(text) = text {
                        if text.is_empty() {
                            if counters.empty_response_retries >= self.options.max_provider_retries
                            {
                                return Err(Error::ProviderRetryExhausted(
                                    "provider returned empty response after max retries".into(),
                                ));
                            }
                            counters.empty_response_retries += 1;
                            self.emit_progress(
                                crate::progress::ProgressUpdate::retrying_empty_response(
                                    counters.empty_response_retries,
                                    self.options.max_provider_retries,
                                ),
                            );
                            continue;
                        }

                        if self.planning.is_active() && !self.plan_exists() {
                            counters.planning_redirects += 1;
                            if counters.planning_redirects
                                >= self.behavior.planning.max_text_redirects_before_auto_plan
                            {
                                self.generate_or_fallback_plan(instruction, ctx.cancel_token())?;
                                self.emit_progress(self.current_working_progress());
                            }
                            self.redirect_to_planning(msg, self.behavior.planning.redirect_message);
                            continue;
                        }

                        self.session.add_message(msg);
                        return Ok(self.finalize_text_response(text, ctx));
                    }
                    self.session.add_message(msg);
                }
                ProviderResponse::ToolCall { id, name, args } => {
                    self.execute_single_tool_call(ctx, instruction, id, name, args)?;
                    counters.empty_response_retries = 0;
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
                    counters.empty_response_retries = 0;
                }
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
    }

    fn finalize_text_response(&mut self, text: String, ctx: &ExecutionContext) -> String {
        // PreFinal hooks: check or annotate before the final response
        self.run_pre_final_hooks(ctx, &text);

        let task_mode = self.task_mode();
        let task_result = self
            .run_state
            .build_task_result(
                &text,
                ctx,
                &ctx.workspace_root,
                &self.behavior,
                &self.generated_tool_warnings,
            )
            .with_task_mode(task_mode);

        let task_result = self.run_bounded_verification_follow_through(task_result, ctx);

        let task_mode = task_result
            .task_mode()
            .unwrap_or(crate::plan::TaskMode::PlanAndExecute);
        let final_response = if self.behavior.should_attach_proof_of_work(
            task_result.files_changed().len(),
            task_result.verification_commands().len(),
            task_result.unresolved_issues().len(),
            self.generated_tool_warnings.len(),
        ) {
            let mut formatted = task_result.format_proof_of_work();
            if self.behavior.should_attach_code_delivery_summary(
                task_mode,
                task_result.files_changed().len(),
                task_result.verification_commands().len(),
            ) {
                if let Some(status) = self.behavior.format_verification_status(
                    task_mode,
                    task_result.files_changed().len(),
                    task_result.verification_commands(),
                ) {
                    formatted = format!("{}\n\n**Delivery Status:** {}", formatted, status);
                }
            }
            formatted
        } else {
            text
        };
        self.last_task_result = Some(task_result);
        final_response
    }

    fn run_bounded_verification_follow_through(
        &mut self,
        mut task_result: crate::task_result::TaskResult,
        ctx: &ExecutionContext,
    ) -> crate::task_result::TaskResult {
        let files_changed = task_result.files_changed();
        let verification_run = task_result.verification_commands();

        if files_changed.is_empty() || !verification_run.is_empty() {
            return task_result;
        }

        if let Some(cmd) = self.suggest_verification_command(&ctx.workspace_root) {
            match Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&ctx.workspace_root)
                .output()
            {
                Ok(output) => {
                    let exit_code = output.status.code().unwrap_or(-1);
                    let verification = VerificationCommand {
                        command: cmd.clone(),
                        output: String::from_utf8_lossy(&output.stdout).to_string(),
                        exit_code,
                        succeeded: exit_code == 0,
                    };
                    task_result = task_result.with_verification_command(verification);
                    tracing::info!("Verification follow-through: {} -> exit {}", cmd, exit_code);
                }
                Err(e) => {
                    tracing::debug!("Verification follow-through skipped: {}", e);
                }
            }
        }

        task_result
    }

    fn suggest_verification_command(&self, _workspace: &std::path::Path) -> Option<String> {
        let candidates = [
            "cargo test --quiet",
            "cargo check --quiet",
            "npm test 2>/dev/null",
            "pnpm test 2>/dev/null",
            "yarn test 2>/dev/null",
            "make test 2>/dev/null",
            "go test ./... 2>/dev/null",
        ];
        for candidate in candidates {
            if let Ok(output) = Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "which {}",
                    candidate.split_whitespace().next().unwrap_or("")
                ))
                .output()
            {
                if output.status.success() {
                    return Some(candidate.to_string());
                }
            }
        }
        None
    }

    fn run_on_session_start_hooks(&mut self, ctx: &ExecutionContext, instruction: &str) {
        let registry = ctx.hook_registry();
        if registry.is_empty() {
            return;
        }
        let input = HookInput {
            event: HookEvent::OnSessionStart,
            subject: String::new(),
            detail: instruction.to_string(),
        };
        let result = dispatch_hooks(
            registry,
            HookEvent::OnSessionStart,
            &input,
            &ctx.workspace_root,
        );
        if let Some(context) = result.annotation_context() {
            self.run_state.record_hook_note(context);
        }
        // OnSessionStart hooks cannot block — they can only annotate.
        // Block verdicts are silently ignored at this boundary.
    }

    fn run_pre_final_hooks(&mut self, ctx: &ExecutionContext, draft_response: &str) {
        let registry = ctx.hook_registry();
        if registry.is_empty() {
            return;
        }
        let input = HookInput {
            event: HookEvent::PreFinal,
            subject: String::new(),
            detail: draft_response.to_string(),
        };
        let result = dispatch_hooks(registry, HookEvent::PreFinal, &input, &ctx.workspace_root);
        if let Some(context) = result.annotation_context() {
            self.run_state.record_hook_note(context);
        }
        // PreFinal hooks can annotate (notes go into proof-of-work) and
        // request verification (recorded as hook notes). Block verdicts
        // are ignored at this boundary — the response has already been
        // generated by the model; the hook can only annotate it.
        for cmd in &result.verify_commands {
            self.run_state
                .record_hook_note(format!("Hook requested verification: {}", cmd));
        }
    }
}

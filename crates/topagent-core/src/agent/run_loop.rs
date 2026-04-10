use super::Agent;
use crate::context::ExecutionContext;
use crate::{Error, Message, ProviderResponse, Result};

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

            if self.planning_gate_active && !self.plan_exists() {
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

                        if self.planning_gate_active && !self.plan_exists() {
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
        let task_result = self.run_state.build_task_result(
            &text,
            ctx,
            &ctx.workspace_root,
            &self.behavior,
            &self.generated_tool_warnings,
        );
        let final_response = if self.behavior.should_attach_proof_of_work(
            task_result.files_changed().len(),
            task_result.verification_commands().len(),
            task_result.unresolved_issues().len(),
            self.generated_tool_warnings.len(),
        ) {
            task_result.format_proof_of_work()
        } else {
            text
        };
        self.last_task_result = Some(task_result);
        final_response
    }
}

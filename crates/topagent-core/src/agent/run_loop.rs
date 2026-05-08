use super::Agent;
use crate::context::ExecutionContext;
use crate::eval::{EvalRecorder, EvalRunRecord};
use crate::harness::AgentPhase;
use crate::task_result::{
    ExecutionSessionOutcome, ToolActionOutcome, ToolActionReceipt, VerificationCommand,
    WorkflowVerification,
};
use crate::{Error, Message, ProviderResponse, Result};
use std::process::Command;
use std::time::{Duration, Instant};

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

        self.eval_model_turns = 0;
        self.eval_skill_calls = 0;
        self.eval_approval_blocks = 0;
        let started_at = Instant::now();
        let result = self.run_inner(ctx, instruction);

        // On any non-Ok exit, capture a partial TaskResult so callers can
        // inspect files changed, bash history, and session outcome even after
        // an interruption. finalize_text_response already sets last_task_result
        // on the Ok path, so we only fill it here when it is still None.
        if result.is_err() && self.last_task_result.is_none() {
            let outcome = match &result {
                Err(Error::Stopped(_)) => ExecutionSessionOutcome::Stopped,
                Err(Error::MaxStepsReached(_)) => ExecutionSessionOutcome::MaxStepsReached,
                _ => ExecutionSessionOutcome::Failed,
            };
            let partial = self
                .run_state
                .build_task_result("", ctx, &ctx.workspace_root, &self.behavior)
                .with_session_outcome(outcome);
            let partial = self.attach_workflow_verification(partial);
            self.last_task_result = Some(partial);
        }

        match &result {
            Ok(_) => self.emit_progress(crate::progress::ProgressUpdate::completed()),
            Err(Error::Stopped(_)) => {
                self.emit_progress(crate::progress::ProgressUpdate::stopped())
            }
            Err(err) => {
                self.emit_progress(crate::progress::ProgressUpdate::failed(err.to_string()))
            }
        }
        self.record_eval_run(ctx, started_at.elapsed(), &result);
        result
    }

    fn record_eval_run(
        &self,
        ctx: &ExecutionContext,
        wall_time: Duration,
        result: &Result<String>,
    ) {
        let Some(path) = self.options.eval_jsonl_path.as_ref() else {
            return;
        };

        let task_id = ctx.task_id().unwrap_or("unscoped").to_string();
        let mut record = EvalRunRecord::new(task_id)
            .with_success(result.is_ok())
            .with_wall_time_ms(duration_millis_u64(wall_time))
            .with_model_turns(self.eval_model_turns)
            .with_skill_calls(self.eval_skill_calls)
            .with_approval_blocks(self.eval_approval_blocks);

        if let Err(err) = result {
            record = record.with_failure(err.to_string());
        }

        if let Some(task_result) = self.last_task_result.as_ref() {
            if let Some(command) = task_result.latest_verification_command() {
                record = record.with_verification_command(command.command.clone());
            }
            record = record.with_files_changed(task_result.files_changed().to_vec());
        }

        if let Err(err) = EvalRecorder::new(path).append(&record) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "failed to append TopAgent eval record"
            );
        }
    }

    fn run_inner(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.check_cancelled(ctx)?;
        self.reset_run_state(ctx, instruction)?;
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
            self.sync_provider_tools(ctx);
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
            self.eval_model_turns = counters.steps;

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
        let task_mode = self.task_mode();
        let task_result = self
            .run_state
            .build_task_result(&text, ctx, &ctx.workspace_root, &self.behavior)
            .with_task_mode(task_mode);

        let task_result = self.run_bounded_verification_follow_through(task_result, ctx);

        let task_result = self.attach_workflow_verification(task_result);

        let task_result = self.compute_delivery_outcome(task_result);

        let task_mode = task_result
            .task_mode()
            .unwrap_or(crate::plan::TaskMode::PlanAndExecute);
        let final_response = if self.behavior.should_attach_proof_of_work(
            task_result.files_changed().len(),
            task_result.verification_commands().len(),
            task_result.unresolved_issues().len(),
        ) {
            // Include the agent's natural response first, then append
            // structured evidence and delivery summary below it. This
            // avoids duplicating the response text inside the evidence
            // and delivery sections.
            let mut formatted = text;
            formatted.push_str("\n\n");
            formatted.push_str(&task_result.format_proof_of_work());
            if self.behavior.should_attach_code_delivery_summary(
                task_mode,
                task_result.files_changed().len(),
                task_result.verification_commands().len(),
            ) {
                if let Some(summary) = task_result.format_delivery_summary() {
                    formatted = format!("{}\n\n{}", formatted, summary);
                }
            }
            formatted
        } else {
            text
        };
        self.last_task_result =
            Some(task_result.with_session_outcome(ExecutionSessionOutcome::Completed));
        final_response
    }

    fn compute_delivery_outcome(
        &self,
        mut task_result: crate::task_result::TaskResult,
    ) -> crate::task_result::TaskResult {
        let files_changed = task_result.files_changed().to_vec();
        let has_verification = !task_result.verification_commands().is_empty();
        let verification_passed = task_result.final_verification_passed();
        let _has_unresolved = task_result.has_unresolved_issues();

        let outcome = if files_changed.is_empty() {
            if has_verification {
                crate::task_result::DeliveryOutcome::AnalysisOnly
            } else {
                crate::task_result::DeliveryOutcome::NoOp
            }
        } else if verification_passed {
            crate::task_result::DeliveryOutcome::CodeChangingVerified
        } else if has_verification {
            crate::task_result::DeliveryOutcome::CodeChangingFailed
        } else {
            crate::task_result::DeliveryOutcome::CodeChangingUnverified
        };

        task_result = task_result.with_delivery_outcome(outcome);
        if files_changed.is_empty() && !has_verification {
            task_result = task_result.with_verification_skip_reason("no files changed".to_string());
        } else if !files_changed.is_empty()
            && !has_verification
            && task_result.verification_skip_reason().is_none()
        {
            task_result =
                task_result.with_verification_skip_reason("verification not attempted".to_string());
        }
        task_result
    }

    fn attach_workflow_verification(
        &self,
        mut task_result: crate::task_result::TaskResult,
    ) -> crate::task_result::TaskResult {
        let task_mode = task_result.task_mode().unwrap_or_else(|| self.task_mode());
        if task_mode != crate::plan::TaskMode::PlanAndExecute {
            return task_result;
        }

        let Some(queue) = self
            .plan
            .lock()
            .ok()
            .map(|plan| plan.queue_status())
            .filter(|queue| queue.total > 0)
        else {
            return task_result;
        };

        let verification_command_count = task_result.verification_commands().len();
        let final_verification_passed = task_result.final_verification_passed();
        let verification_satisfied = !task_result.has_files_changed() || final_verification_passed;
        let queue_complete = queue.is_complete();
        let satisfied = queue_complete && verification_satisfied;
        let summary = workflow_verification_summary(
            queue,
            verification_command_count,
            final_verification_passed,
            satisfied,
        );

        if !satisfied {
            let issue = format!("Workflow incomplete: {summary}");
            if !task_result.unresolved_issues().contains(&issue) {
                task_result = task_result.with_unresolved_issue(issue);
            }
        }

        task_result.with_workflow_verification(WorkflowVerification {
            queue,
            verification_command_count,
            final_verification_passed,
            satisfied,
            summary,
        })
    }

    fn run_bounded_verification_follow_through(
        &mut self,
        mut task_result: crate::task_result::TaskResult,
        ctx: &ExecutionContext,
    ) -> crate::task_result::TaskResult {
        let files_changed = task_result.files_changed();
        let verification_run = task_result.verification_commands();

        if files_changed.is_empty() {
            return task_result;
        }

        if !verification_run.is_empty() {
            return task_result;
        }

        if let Some(cmd) = self.suggest_verification_command(&ctx.workspace_root) {
            let input = serde_json::json!({ "command": cmd.clone() });
            match self.harness.execute_skill(
                "bash",
                input.clone(),
                AgentPhase::Verify,
                ctx,
                &self.options,
            ) {
                Ok(execution) => {
                    let receipt = self.record_tool_action_receipt(
                        ctx,
                        ToolActionReceipt::new(
                            "bash",
                            AgentPhase::Verify.as_str(),
                            true,
                            ToolActionOutcome::Succeeded,
                            Self::tool_receipt_summary("bash", &input),
                        )
                        .with_effects(execution.effects)
                        .with_risk(execution.risk),
                    );
                    task_result = task_result.with_tool_receipt(receipt);
                    let output = ctx.secrets().redact(&execution.output).into_owned();
                    let exit_code = super::extract_exit_code(&output);
                    let verification = VerificationCommand {
                        command: cmd.clone(),
                        output,
                        exit_code,
                        succeeded: exit_code == 0,
                    };
                    task_result = task_result.with_verification_command(verification);
                    tracing::info!("Verification follow-through: {} -> exit {}", cmd, exit_code);
                }
                Err(Error::ApprovalRequired(request)) => {
                    let receipt = self.record_tool_action_receipt(
                        ctx,
                        ToolActionReceipt::new(
                            "bash",
                            AgentPhase::Verify.as_str(),
                            false,
                            ToolActionOutcome::Blocked,
                            format!("approval required: {}", request.short_summary),
                        ),
                    );
                    task_result = task_result.with_tool_receipt(receipt);
                    task_result = task_result.with_verification_skip_reason(format!(
                        "approval required for verification follow-through: {}",
                        request.short_summary
                    ));
                }
                Err(Error::Capability(error)) => {
                    let receipt = self.record_tool_action_receipt(
                        ctx,
                        ToolActionReceipt::new(
                            "bash",
                            AgentPhase::Verify.as_str(),
                            false,
                            ToolActionOutcome::Blocked,
                            format!("capability blocked verification follow-through: {error}"),
                        ),
                    );
                    task_result = task_result.with_tool_receipt(receipt);
                    task_result = task_result
                        .with_verification_skip_reason(format!("capability blocked: {error}"));
                }
                Err(error) => {
                    let receipt = self.record_tool_action_receipt(
                        ctx,
                        ToolActionReceipt::new(
                            "bash",
                            AgentPhase::Verify.as_str(),
                            false,
                            ToolActionOutcome::Failed,
                            format!("verification follow-through failed: {error}"),
                        ),
                    );
                    task_result = task_result.with_tool_receipt(receipt);
                    tracing::debug!("Verification follow-through skipped: {}", error);
                    task_result = task_result
                        .with_verification_skip_reason(format!("command not available: {}", error));
                }
            }
        } else {
            task_result = task_result.with_verification_skip_reason(
                "no obvious verification command available".to_string(),
            );
        }

        task_result
    }

    fn suggest_verification_command(&self, workspace: &std::path::Path) -> Option<String> {
        let candidates = [
            ("cargo test --quiet", "Cargo.toml"),
            ("cargo check --quiet", "Cargo.toml"),
            ("npm test 2>/dev/null", "package.json"),
            ("pnpm test 2>/dev/null", "package.json"),
            ("yarn test 2>/dev/null", "package.json"),
            ("make test 2>/dev/null", "Makefile"),
            ("go test ./... 2>/dev/null", "go.mod"),
        ];
        for (candidate, marker_file) in candidates {
            // Only suggest a verification command if the workspace actually
            // contains the matching build system marker file. This prevents
            // running cargo test in a non-Rust workspace, etc.
            if !workspace.join(marker_file).exists() {
                continue;
            }
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
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn workflow_verification_summary(
    queue: crate::plan::TaskQueueStatus,
    verification_command_count: usize,
    final_verification_passed: bool,
    satisfied: bool,
) -> String {
    if satisfied {
        return format!(
            "plan complete ({}/{} done) and verification satisfied",
            queue.done, queue.total
        );
    }

    let mut parts = Vec::new();
    if !queue.is_complete() {
        parts.push(format!(
            "plan incomplete: {}/{} done, {} pending, {} active, {} blocked",
            queue.done, queue.total, queue.pending, queue.in_progress, queue.blocked
        ));
    }
    if verification_command_count == 0 {
        parts.push("verification missing".to_string());
    } else if !final_verification_passed {
        parts.push("final verification did not pass".to_string());
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ReadTool;
    use crate::Agent;
    use tempfile::TempDir;

    fn minimal_agent() -> Agent {
        let provider = crate::ScriptedProvider::new(vec![]);
        Agent::new(Box::new(provider), vec![Box::new(ReadTool::new())])
    }

    #[test]
    fn test_suggest_verification_returns_none_when_no_marker_file() {
        let temp = TempDir::new().unwrap();
        let agent = minimal_agent();
        let result = agent.suggest_verification_command(temp.path());
        assert!(
            result.is_none(),
            "should return None when no build system marker exists, got: {:?}",
            result
        );
    }

    #[test]
    fn test_suggest_verification_returns_some_when_cargo_toml_present() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\n").unwrap();
        let agent = minimal_agent();
        let result = agent.suggest_verification_command(temp.path());
        // Result depends on whether `cargo` is on PATH (it should be in
        // a Rust development environment).
        if which_exists("cargo") {
            assert!(
                result.is_some(),
                "should suggest cargo test when Cargo.toml exists and cargo is on PATH"
            );
            let cmd = result.unwrap();
            assert!(
                cmd.contains("cargo"),
                "suggested command should be cargo-based, got: {}",
                cmd
            );
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_suggest_verification_skips_non_matching_markers() {
        let temp = TempDir::new().unwrap();
        // Only package.json exists, no Cargo.toml
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        let agent = minimal_agent();
        let result = agent.suggest_verification_command(temp.path());
        // Should NOT suggest cargo test since Cargo.toml is absent
        if let Some(cmd) = result {
            assert!(
                !cmd.contains("cargo"),
                "should not suggest cargo when Cargo.toml is missing"
            );
        }
    }

    fn which_exists(cmd: &str) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("which {}", cmd))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

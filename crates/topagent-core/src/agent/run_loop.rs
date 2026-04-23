use super::Agent;
use crate::context::ExecutionContext;
use crate::task_result::{ExecutionSessionOutcome, VerificationCommand};
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
        result
    }

    fn run_inner(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.check_cancelled(ctx)?;
        self.reset_run_state(ctx, instruction)?;
        self.sync_provider_tools();
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
        let task_mode = self.task_mode();
        let task_result = self
            .run_state
            .build_task_result(&text, ctx, &ctx.workspace_root, &self.behavior)
            .with_task_mode(task_mode);

        let task_result = self.run_bounded_verification_follow_through(task_result, ctx);

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
                    task_result = task_result
                        .with_verification_skip_reason(format!("command not available: {}", e));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Agent;
    use crate::tools::ReadTool;
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

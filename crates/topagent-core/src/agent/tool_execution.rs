use super::{extract_exit_code, Agent};
use crate::behavior::BashCommandClass;
use crate::checkpoint::WorkspaceCheckpointStatus;
use crate::context::{ExecutionContext, ToolContext};
use crate::external::ExternalToolEffect;
use crate::provenance::fetched_content_source;
use crate::tools::risky_shell_changed_path_hints;
use crate::{Message, ProgressUpdate, Result};

impl Agent {
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

    pub(super) fn execute_single_tool_call(
        &mut self,
        ctx: &ExecutionContext,
        instruction: &str,
        id: String,
        name: String,
        args: serde_json::Value,
    ) -> Result<()> {
        let is_external = self.external_tools.get(&name).is_some();

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

        self.run_state.track_active_file(&name, &args);
        let bash_args = if name == "bash" { Some(&args) } else { None };
        if let Some(block) = self.run_preflight(ctx, &name, &args, bash_args, None, None)? {
            self.record_tool_result(id, name, args, block.message);
            if block.is_planning_block {
                self.note_planning_block(ctx, instruction)?;
            }
            return Ok(());
        }

        let tool = self.tools.get(&name).unwrap();
        let changed_before = self.run_state.changed_files();
        let checkpoint_status_before = if name == "bash" {
            ctx.checkpoint_store()
                .and_then(|store| store.latest_status().ok().flatten())
        } else {
            None
        };

        let tool_ctx = ToolContext::new(ctx, &self.options);
        let bash_cmd = if name == "bash" {
            Some(Self::extract_bash_command(&args))
        } else {
            None
        };
        let mut bash_exit_code = None;
        self.emit_progress(self.tool_progress(&name, &args));
        self.check_cancelled(ctx)?;
        let raw_result = match tool.execute(args.clone(), &tool_ctx) {
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

        let mut execution_started_by_bash = false;
        if let Some(cmd) = bash_cmd.as_ref() {
            bash_exit_code = Some(extract_exit_code(&raw_result));
            if name == "bash" {
                self.run_state
                    .record_tool_trace(&name, &args, Some(cmd), &self.behavior);
            }
        } else {
            self.run_state
                .record_tool_trace(&name, &args, None, &self.behavior);
        }

        if name == "bash" {
            let mut found_new_change = false;
            let class = if let Some(cmd_str) = &bash_cmd {
                Self::classify_bash_command(cmd_str)
            } else {
                BashCommandClass::MutationRisk
            };
            if matches!(
                class,
                BashCommandClass::MutationRisk | BashCommandClass::Verification
            ) {
                found_new_change = self.run_state.reconcile_changed_files(&ctx.workspace_root);
                if found_new_change {
                    execution_started_by_bash = true;
                }
            }
            if class == BashCommandClass::MutationRisk {
                if !found_new_change {
                    if let Some(cmd) = bash_cmd.as_deref() {
                        let hinted_paths = risky_shell_changed_path_hints(cmd);
                        if self.run_state.track_inferred_changed_paths(&hinted_paths) {
                            found_new_change = true;
                        }
                    }
                }
                execution_started_by_bash = true;
            }
            if found_new_change {
                self.maybe_escalate_to_planning();
            }
        }

        if execution_started_by_bash {
            self.mark_execution_started();
        }

        self.run_state.track_changed_file(&name, &args);

        if self.behavior.is_mutation_tool(&name) {
            self.run_state.reconcile_changed_files(&ctx.workspace_root);
            self.mark_execution_started();
            self.maybe_escalate_to_planning();
        }

        if self.behavior.is_planning_tool(&name) && self.plan_exists() {
            self.deactivate_planning_gate();
        }

        if self.behavior.is_memory_write_tool(&name) {
            self.durable_memory_written_this_run = true;
        }

        if self.behavior.mutates_generated_tool_surface(&name) {
            self.reload_workspace_tools(&ctx.workspace_root)?;
        }

        let result = match ctx.secrets().redact(&raw_result) {
            std::borrow::Cow::Owned(s) => s,
            std::borrow::Cow::Borrowed(_) => raw_result,
        };

        if let (Some(cmd), Some(exit_code)) = (bash_cmd.as_ref(), bash_exit_code) {
            self.run_state
                .record_bash_result(cmd.clone(), result.clone(), exit_code);
            if let Some(source) = fetched_content_source(cmd) {
                self.run_state.record_observed_source(source);
            }
        }

        self.emit_post_tool_progress(
            ctx,
            &name,
            &args,
            bash_cmd.as_deref(),
            bash_exit_code,
            &changed_before,
            checkpoint_status_before.as_ref(),
        );
        self.record_tool_result(id, name, args, result);
        Ok(())
    }

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
        let changed_before = self.run_state.changed_files();

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

        self.emit_progress(Self::external_tool_progress(&name, external_effect));
        self.check_cancelled(ctx)?;
        let tool_ctx = ToolContext::new(ctx, &self.options);
        let external_tool = self.external_tools.get(&name).unwrap();
        let result = external_tool.execute(&args, &tool_ctx);
        self.check_cancelled(ctx)?;
        let found_new_change = self.run_state.reconcile_changed_files(&ctx.workspace_root);
        if found_new_change && self.execution_stage == super::ExecutionStage::Research {
            self.execution_stage = super::ExecutionStage::Edit;
        }
        if found_new_change {
            self.maybe_escalate_to_planning();
        }

        let result_str = match result {
            Ok(r) => {
                self.run_state
                    .record_tool_trace(&name, &args, None, &self.behavior);
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
        self.emit_external_tool_post_progress(&changed_before);
        self.record_tool_result(id, name, args, result_str);
        Ok(())
    }

    fn tool_progress(&self, name: &str, args: &serde_json::Value) -> ProgressUpdate {
        if self.behavior.is_planning_tool(name) {
            return ProgressUpdate::planning();
        }

        if name == "read" {
            if let Some(path) = Self::extract_file_path(args) {
                return ProgressUpdate::working(format!("Reading file: {}", path));
            }
        }

        if matches!(name, "write" | "edit") {
            if let Some(path) = Self::extract_file_path(args) {
                return ProgressUpdate::working(format!("Editing file: {}", path));
            }
        }

        if name == "bash" {
            let bash_cmd = Self::extract_bash_command(args);
            return match Self::classify_bash_command(&bash_cmd) {
                BashCommandClass::Verification => ProgressUpdate::working(format!(
                    "Running verification: {}",
                    Self::summarize_progress_text(&bash_cmd, 96)
                )),
                _ => ProgressUpdate::running_tool("bash"),
            };
        }

        ProgressUpdate::running_tool(name)
    }

    fn external_tool_progress(name: &str, effect: ExternalToolEffect) -> ProgressUpdate {
        match effect {
            ExternalToolEffect::VerificationOnly => {
                ProgressUpdate::working(format!("Running verification tool: {}", name))
            }
            ExternalToolEffect::ExecutionStarted => {
                ProgressUpdate::working(format!("Running execution tool: {}", name))
            }
            ExternalToolEffect::ReadOnly => ProgressUpdate::running_tool(name),
        }
    }

    fn extract_file_path(args: &serde_json::Value) -> Option<&str> {
        args.get("path").and_then(|value| value.as_str())
    }

    fn extract_bash_command(args: &serde_json::Value) -> String {
        args.get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    fn summarize_progress_text(text: &str, max_chars: usize) -> String {
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.len() <= max_chars {
            return compact;
        }

        let mut end = max_chars;
        while end > 0 && !compact.is_char_boundary(end) {
            end -= 1;
        }
        let mut limited = compact[..end].trim_end().to_string();
        limited.push_str("...");
        limited
    }

    fn changed_files_progress_update(files: &[String]) -> Option<ProgressUpdate> {
        match files {
            [] => None,
            [path] => Some(ProgressUpdate::working(format!("Changed file: {}", path))),
            _ => Some(ProgressUpdate::working(format!(
                "Changed files: {}",
                Self::summarize_progress_text(&files.join(", "), 96)
            ))),
        }
    }

    fn new_changed_files<'a>(before: &[String], after: &'a [String]) -> Vec<&'a String> {
        after.iter().filter(|path| !before.contains(path)).collect()
    }

    fn verification_progress_update(command: &str, exit_code: i32) -> ProgressUpdate {
        let command = Self::summarize_progress_text(command, 96);
        if exit_code == 0 {
            ProgressUpdate::working(format!("Verification passed: {}", command))
        } else {
            ProgressUpdate::working(format!(
                "Verification failed (exit {}): {}",
                exit_code, command
            ))
        }
    }

    fn bash_checkpoint_progress_update(
        &self,
        ctx: &ExecutionContext,
        before: Option<&WorkspaceCheckpointStatus>,
    ) -> Option<ProgressUpdate> {
        let after = ctx.checkpoint_store()?.latest_status().ok().flatten()?;
        let before_count = before.map_or(0, |status| status.captures.len());
        if after.captures.len() <= before_count {
            return None;
        }

        let capture = after.captures.last()?;
        let detail = capture.detail.as_deref().unwrap_or(capture.reason.as_str());
        Some(ProgressUpdate::working(format!(
            "Checkpointed workspace before risky shell command: {}",
            Self::summarize_progress_text(detail, 96)
        )))
    }

    fn emit_post_tool_progress(
        &self,
        ctx: &ExecutionContext,
        name: &str,
        args: &serde_json::Value,
        bash_cmd: Option<&str>,
        bash_exit_code: Option<i32>,
        changed_before: &[String],
        checkpoint_status_before: Option<&WorkspaceCheckpointStatus>,
    ) {
        if matches!(name, "write" | "edit") {
            if let Some(path) = Self::extract_file_path(args) {
                self.emit_progress(ProgressUpdate::working(format!("Changed file: {}", path)));
                return;
            }
        }

        if name == "bash" {
            if let Some(command) = bash_cmd {
                match Self::classify_bash_command(command) {
                    BashCommandClass::Verification => {
                        if let Some(exit_code) = bash_exit_code {
                            self.emit_progress(Self::verification_progress_update(
                                command, exit_code,
                            ));
                        }
                    }
                    BashCommandClass::MutationRisk => {
                        if let Some(update) =
                            self.bash_checkpoint_progress_update(ctx, checkpoint_status_before)
                        {
                            self.emit_progress(update);
                        }
                        let changed_after = self.run_state.changed_files();
                        let new_files = Self::new_changed_files(changed_before, &changed_after)
                            .into_iter()
                            .cloned()
                            .collect::<Vec<_>>();
                        if let Some(update) = Self::changed_files_progress_update(&new_files) {
                            self.emit_progress(update);
                        }
                    }
                    BashCommandClass::ResearchSafe => {}
                }
            }
        }
    }

    fn emit_external_tool_post_progress(&self, changed_before: &[String]) {
        let changed_after = self.run_state.changed_files();
        let new_files = Self::new_changed_files(changed_before, &changed_after)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(update) = Self::changed_files_progress_update(&new_files) {
            self.emit_progress(update);
        }
    }
}

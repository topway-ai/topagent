use super::Agent;
use crate::approval::ApprovalCheck;
use crate::behavior::PreExecutionState;
use crate::context::ExecutionContext;
use crate::progress::ProgressUpdate;
use crate::{Error, Result};

pub(super) struct PreflightBlock {
    pub(super) message: String,
    pub(super) is_planning_block: bool,
}

impl Agent {
    pub(super) fn run_preflight(
        &mut self,
        ctx: &ExecutionContext,
        name: &str,
        args: &serde_json::Value,
        bash_args: Option<&serde_json::Value>,
    ) -> Result<Option<PreflightBlock>> {
        if let Some(block_msg) = self.check_planning_gate(name, bash_args) {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Ok(Some(PreflightBlock {
                message: block_msg,
                is_planning_block: true,
            }));
        }
        if let Some(block_msg) = self.check_pre_execution_verification_gate(name, bash_args) {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Ok(Some(PreflightBlock {
                message: block_msg,
                is_planning_block: false,
            }));
        }
        if let Some(block_msg) = self.check_memory_trust_gate(ctx, name) {
            self.emit_progress(Self::blocked_progress(&block_msg));
            return Ok(Some(PreflightBlock {
                message: format!("error: {block_msg}"),
                is_planning_block: false,
            }));
        }
        if let Some(block) = self.check_approval_gate(ctx, name, args, bash_args)? {
            return Ok(Some(block));
        }

        Ok(None)
    }

    fn blocked_progress(reason: &str) -> ProgressUpdate {
        if reason.contains("Planning required") {
            ProgressUpdate::blocked("Blocked: planning required before mutation.")
        } else {
            ProgressUpdate::blocked(format!("Blocked: {}", reason))
        }
    }

    fn check_pre_execution_verification_gate(
        &self,
        tool_name: &str,
        bash_args: Option<&serde_json::Value>,
    ) -> Option<String> {
        let bash_command = bash_args
            .and_then(|args| args.get("command"))
            .and_then(|value| value.as_str());

        self.behavior.pre_execution_block_message(
            tool_name,
            bash_command,
            &PreExecutionState {
                planning_required_for_task: self.planning.is_required_for_task(),
                plan_exists: self.plan_exists(),
                execution_started: self.execution_started(),
                task_mode: self.planning.task_mode(),
            },
        )
    }

    fn check_memory_trust_gate(&self, ctx: &ExecutionContext, tool_name: &str) -> Option<String> {
        if !self.behavior.is_memory_write_tool(tool_name) {
            return None;
        }

        self.behavior.memory_write_block_reason(
            tool_name,
            &self.run_state.trust_context(ctx),
            self.run_state
                .has_trusted_local_corroboration(&self.behavior),
        )
    }

    fn check_approval_gate(
        &self,
        ctx: &ExecutionContext,
        tool_name: &str,
        args: &serde_json::Value,
        bash_args: Option<&serde_json::Value>,
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
            Some(&self.run_state.trust_context(ctx)),
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

    fn check_planning_gate(
        &self,
        tool_name: &str,
        bash_args: Option<&serde_json::Value>,
    ) -> Option<String> {
        if !self.planning.is_active() {
            return None;
        }
        let bash_command = bash_args
            .and_then(|args| args.get("command"))
            .and_then(|value| value.as_str());

        self.behavior
            .planning_block_message(tool_name, bash_command, self.plan_exists())
    }
}

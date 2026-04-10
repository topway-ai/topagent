use super::BehaviorContract;
use crate::approval::{
    ApprovalEnforcement, ApprovalPolicy, ApprovalRequestDraft, ApprovalTriggerKind,
    ApprovalTriggerRule,
};
use crate::command_exec::CommandSandboxPolicy;
use crate::external::ExternalToolEffect;
use crate::provenance::RunTrustContext;

pub(super) fn default_approval_policy() -> ApprovalPolicy {
    ApprovalPolicy {
        mailbox_available: false,
        triggers: &[
            ApprovalTriggerRule {
                kind: ApprovalTriggerKind::GitCommit,
                label: "git_commit",
                enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                rationale: "commits publish a durable repo milestone",
            },
            ApprovalTriggerRule {
                kind: ApprovalTriggerKind::DestructiveShellMutation,
                label: "shell mutation",
                enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                rationale: "shell mutations can bypass safer structured tools",
            },
            ApprovalTriggerRule {
                kind: ApprovalTriggerKind::HostExternalExecution,
                label: "host-sandbox external tool execution",
                enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                rationale: "host tools reach beyond the workspace sandbox",
            },
            ApprovalTriggerRule {
                kind: ApprovalTriggerKind::GeneratedToolDeletion,
                label: "delete_generated_tool",
                enforcement: ApprovalEnforcement::RequiredWhenAvailable,
                rationale: "tool deletion removes workspace-local operator tooling",
            },
        ],
    }
}

impl ApprovalPolicy {
    fn build_request(
        &self,
        kind: ApprovalTriggerKind,
        short_summary: String,
        exact_action: String,
        scope_of_impact: String,
        expected_effect: String,
        rollback_hint: Option<String>,
        low_trust_summary: Option<&str>,
    ) -> Option<ApprovalRequestDraft> {
        let rule = self.triggers.iter().find(|rule| rule.kind == kind)?;
        if rule.enforcement == ApprovalEnforcement::AdvisoryOnly {
            return None;
        }
        let reason = match low_trust_summary {
            Some(summary) => format!(
                "{} Proposed action is influenced by low-trust content from: {}.",
                rule.rationale, summary
            ),
            None => rule.rationale.to_string(),
        };
        Some(ApprovalRequestDraft {
            action_kind: kind,
            short_summary,
            exact_action,
            reason,
            scope_of_impact,
            expected_effect,
            rollback_hint,
        })
    }
}

impl BehaviorContract {
    pub fn approval_request(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        bash_command: Option<&str>,
        external_effect: Option<ExternalToolEffect>,
        external_sandbox: Option<CommandSandboxPolicy>,
        trust_context: Option<&RunTrustContext>,
    ) -> Option<ApprovalRequestDraft> {
        let low_trust_summary = trust_context.and_then(|trust| trust.low_trust_action_summary(2));

        if tool_name == "git_commit" {
            let message = args
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing commit message>");
            return self.approval.build_request(
                ApprovalTriggerKind::GitCommit,
                format!("git commit: {}", compact_action_text(message, 80)),
                format!("git_commit(message={message:?})"),
                "Creates a new git commit in the current workspace repository.".to_string(),
                "Staged changes become a durable repo milestone.".to_string(),
                Some("Use git revert or git reset if the commit needs to be undone.".to_string()),
                low_trust_summary.as_deref(),
            );
        }

        if tool_name == "bash" {
            let command = bash_command?;
            if self.classify_bash_command(command) != super::BashCommandClass::MutationRisk {
                return None;
            }

            return self.approval.build_request(
                ApprovalTriggerKind::DestructiveShellMutation,
                format!("bash mutation: {}", compact_action_text(command.trim(), 90)),
                command.trim().to_string(),
                "May create, overwrite, move, or delete files outside structured edit tools."
                    .to_string(),
                "Runs a filesystem-changing shell command directly through the shell.".to_string(),
                Some(
                    "Use `topagent checkpoint restore` for the latest workspace checkpoint, then inspect git diff for any remaining shell-side effects."
                        .to_string(),
                ),
                low_trust_summary.as_deref(),
            );
        }

        if external_sandbox == Some(CommandSandboxPolicy::Host) {
            let effect = match external_effect.unwrap_or(ExternalToolEffect::ReadOnly) {
                ExternalToolEffect::ReadOnly => {
                    "Runs a host-scoped external tool outside the workspace sandbox."
                }
                ExternalToolEffect::VerificationOnly => {
                    "Runs a host-scoped verification tool outside the workspace sandbox."
                }
                ExternalToolEffect::ExecutionStarted => {
                    "Runs a host-scoped execution tool outside the workspace sandbox."
                }
            };
            return self.approval.build_request(
                ApprovalTriggerKind::HostExternalExecution,
                format!("host external tool: {tool_name}"),
                format!("{tool_name}({})", compact_json(args)),
                "May reach beyond the workspace sandbox and affect host-visible state.".to_string(),
                effect.to_string(),
                None,
                low_trust_summary.as_deref(),
            );
        }

        if tool_name == "delete_generated_tool" {
            let name = args
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing tool name>");
            return self.approval.build_request(
                ApprovalTriggerKind::GeneratedToolDeletion,
                format!("delete generated tool: {name}"),
                format!("delete_generated_tool(name={name:?})"),
                "Removes a workspace-local tool from .topagent/tools/.".to_string(),
                "Deletes the generated tool until it is recreated.".to_string(),
                Some("Use create_tool or repair_tool to restore the tool later.".to_string()),
                low_trust_summary.as_deref(),
            );
        }

        None
    }
}

fn compact_action_text(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= limit {
        compact
    } else {
        format!("{}...", &compact[..limit.saturating_sub(3)])
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    compact_action_text(&rendered, 100)
}

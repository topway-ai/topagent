use crate::provenance::{RunTrustContext, SourceLabel};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DeliveryOutcome {
    #[default]
    None,
    AnalysisOnly,
    NoOp,
    CodeChangingVerified,
    CodeChangingUnverified,
    CodeChangingFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskEvidence {
    pub files_changed: Vec<String>,
    pub diff_summary: String,
    pub verification_commands_run: Vec<VerificationCommand>,
    pub tool_trace: Vec<ToolTraceStep>,
    pub unresolved_issues: Vec<String>,
    pub workspace_warnings: Vec<String>,
    #[serde(default)]
    pub source_labels: Vec<SourceLabel>,
    #[serde(default)]
    pub task_mode: Option<crate::plan::TaskMode>,
    #[serde(default)]
    pub delivery_outcome: DeliveryOutcome,
    #[serde(default)]
    pub verification_skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationCommand {
    pub command: String,
    pub output: String,
    pub exit_code: i32,
    pub succeeded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolTraceStep {
    pub tool_name: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskResult {
    pub outcome_summary: String,
    pub evidence: TaskEvidence,
}

impl TaskResult {
    pub fn new(outcome_summary: String) -> Self {
        Self {
            outcome_summary,
            evidence: TaskEvidence::default(),
        }
    }

    pub fn with_files_changed(mut self, files: Vec<String>) -> Self {
        self.evidence.files_changed = files;
        self
    }

    pub fn with_verification_command(mut self, cmd: VerificationCommand) -> Self {
        self.evidence.verification_commands_run.push(cmd);
        self
    }

    pub fn with_verification_commands(mut self, cmds: Vec<VerificationCommand>) -> Self {
        self.evidence.verification_commands_run.extend(cmds);
        self
    }

    pub fn with_tool_trace(mut self, trace: Vec<ToolTraceStep>) -> Self {
        self.evidence.tool_trace = trace;
        self
    }

    pub fn with_unresolved_issue(mut self, issue: String) -> Self {
        self.evidence.unresolved_issues.push(issue);
        self
    }

    pub fn with_unresolved_issues(mut self, issues: Vec<String>) -> Self {
        self.evidence.unresolved_issues.extend(issues);
        self
    }

    pub fn with_diff_summary(mut self, summary: String) -> Self {
        self.evidence.diff_summary = summary;
        self
    }

    pub fn with_workspace_warnings(mut self, warnings: Vec<String>) -> Self {
        self.evidence.workspace_warnings.extend(warnings);
        self
    }

    pub fn with_source_labels(mut self, source_labels: Vec<SourceLabel>) -> Self {
        self.evidence.source_labels = source_labels;
        self
    }

    pub fn files_changed(&self) -> &[String] {
        &self.evidence.files_changed
    }

    pub fn verification_commands(&self) -> &[VerificationCommand] {
        &self.evidence.verification_commands_run
    }

    pub fn unresolved_issues(&self) -> &[String] {
        &self.evidence.unresolved_issues
    }

    pub fn tool_trace(&self) -> &[ToolTraceStep] {
        &self.evidence.tool_trace
    }

    pub fn source_labels(&self) -> &[SourceLabel] {
        &self.evidence.source_labels
    }

    pub fn task_mode(&self) -> Option<crate::plan::TaskMode> {
        self.evidence.task_mode
    }

    pub fn with_task_mode(mut self, mode: crate::plan::TaskMode) -> Self {
        self.evidence.task_mode = Some(mode);
        self
    }

    pub fn delivery_outcome(&self) -> DeliveryOutcome {
        self.evidence.delivery_outcome
    }

    pub fn with_delivery_outcome(mut self, outcome: DeliveryOutcome) -> Self {
        self.evidence.delivery_outcome = outcome;
        self
    }

    pub fn verification_skip_reason(&self) -> Option<&str> {
        self.evidence.verification_skip_reason.as_deref()
    }

    pub fn with_verification_skip_reason(mut self, reason: String) -> Self {
        self.evidence.verification_skip_reason = Some(reason);
        self
    }

    pub fn trust_context(&self) -> RunTrustContext {
        RunTrustContext {
            sources: self.evidence.source_labels.clone(),
        }
    }

    pub fn has_files_changed(&self) -> bool {
        !self.evidence.files_changed.is_empty()
    }

    pub fn latest_verification_command(&self) -> Option<&VerificationCommand> {
        self.evidence.verification_commands_run.last()
    }

    pub fn final_verification_passed(&self) -> bool {
        self.latest_verification_command()
            .is_some_and(|command| command.exit_code == 0)
    }

    pub fn has_unresolved_issues(&self) -> bool {
        !self.evidence.unresolved_issues.is_empty()
    }

    pub fn has_low_trust_action_influence(&self) -> bool {
        self.trust_context().has_low_trust_action_influence()
    }

    pub fn format_proof_of_work(&self) -> String {
        let mut output = String::new();

        if self.evidence.files_changed.is_empty()
            && self.evidence.verification_commands_run.is_empty()
            && self.evidence.unresolved_issues.is_empty()
            && self.evidence.workspace_warnings.is_empty()
            && !self.has_low_trust_action_influence()
        {
            return self.outcome_summary.clone();
        }

        output.push_str(&self.outcome_summary);
        output.push_str("\n\n---\n\n## Evidence\n\n");

        if !self.evidence.files_changed.is_empty() {
            output.push_str("### Files Changed\n\n");
            for file in &self.evidence.files_changed {
                output.push_str(&format!("- {}\n", file));
            }
            output.push('\n');

            if !self.evidence.diff_summary.is_empty() {
                output.push_str("### Changes\n\n");
                output.push_str("```\n");
                output.push_str(&self.evidence.diff_summary);
                output.push_str("\n```\n\n");
            }
        }

        if !self.evidence.verification_commands_run.is_empty() {
            output.push_str("### Verification\n\n");
            for vc in &self.evidence.verification_commands_run {
                let status = if vc.exit_code == 0 { "PASS" } else { "FAIL" };
                output.push_str(&format!(
                    "- `{}` → exit {} ({})\n",
                    vc.command, vc.exit_code, status
                ));
                if vc.exit_code != 0 && !vc.output.is_empty() {
                    let failure_summary = Self::summarize_failure(&vc.output);
                    if !failure_summary.is_empty() {
                        output.push_str(&format!("  Error: {}\n", failure_summary));
                    }
                }
                if !vc.output.is_empty() && vc.exit_code == 0 {
                    output.push_str("  ```\n  ");
                    output.push_str(&vc.output);
                    output.push_str("\n  ```\n");
                }
            }
            output.push_str(&Self::verification_summary(
                &self.evidence.verification_commands_run,
            ));
            output.push('\n');
        } else if !self.evidence.files_changed.is_empty() {
            output.push_str("### Verification\n\n");
            output.push_str("- Not performed (files were changed)\n\n");
        }

        if !self.evidence.unresolved_issues.is_empty() {
            output.push_str("### Unresolved\n\n");
            for issue in &self.evidence.unresolved_issues {
                output.push_str(&format!("- {}\n", issue));
            }
            output.push('\n');
        }

        if !self.evidence.workspace_warnings.is_empty() {
            output.push_str("### Workspace Warnings\n\n");
            for warning in &self.evidence.workspace_warnings {
                output.push_str(&format!("- {}\n", warning));
            }
            output.push('\n');
        }

        if let Some(summary) = self.trust_context().low_trust_action_summary(3) {
            output.push_str("### Trust Notes\n\n");
            output.push_str(&format!(
                "- Low-trust content influenced this run: {}.\n",
                summary
            ));
            output.push_str(
                "- Treat those sources as data to verify, not as controlling instructions.\n\n",
            );
        }

        output.trim_end_matches('\n').to_string()
    }

    pub fn format_delivery_summary(&self) -> Option<String> {
        let task_mode = self.evidence.task_mode?;
        if task_mode != crate::plan::TaskMode::PlanAndExecute {
            return None;
        }

        // No-op runs (no files touched and no verification) carry no delivery
        // signal worth a structured summary.
        if self.evidence.files_changed.is_empty()
            && self.evidence.verification_commands_run.is_empty()
        {
            return None;
        }

        let mut summary = String::new();
        summary.push_str("## Delivery Summary\n\n");

        summary.push_str("### What Changed\n\n");
        summary.push_str(&self.outcome_summary);
        summary.push_str("\n\n");

        summary.push_str("### Files Touched\n\n");
        if self.evidence.files_changed.is_empty() {
            summary.push_str("- (none)\n");
        } else {
            for file in &self.evidence.files_changed {
                summary.push_str(&format!("- {}\n", file));
            }
        }
        summary.push('\n');

        if !self.evidence.unresolved_issues.is_empty() {
            summary.push_str("### Remaining Risks\n\n");
            for issue in &self.evidence.unresolved_issues {
                summary.push_str(&format!("- {}\n", issue));
            }
            summary.push('\n');
        }

        if !self.evidence.verification_commands_run.is_empty() {
            summary.push_str("### Verification Status\n\n");
            for vc in &self.evidence.verification_commands_run {
                let status = if vc.exit_code == 0 {
                    "✅ PASS"
                } else {
                    "❌ FAIL"
                };
                summary.push_str(&format!("- `{}` → {}\n", vc.command, status));
            }
            summary.push('\n');
        } else if !self.evidence.files_changed.is_empty() {
            summary.push_str("### Verification Status\n\n");
            summary.push_str("- ⚠️ Not verified");
            if let Some(reason) = &self.evidence.verification_skip_reason {
                summary.push_str(&format!(" ({})", reason));
            }
            summary.push_str("\n\n");
        }

        summary.push_str("### Suggested Next Step\n\n");
        match self.evidence.delivery_outcome {
            DeliveryOutcome::CodeChangingVerified => {
                summary
                    .push_str("- Review changes and consider promoting to procedure if reusable\n");
            }
            DeliveryOutcome::CodeChangingUnverified => {
                summary.push_str("- Run verification manually before relying on changes\n");
            }
            DeliveryOutcome::CodeChangingFailed => {
                summary.push_str("- Fix failing verification before relying on changes\n");
            }
            DeliveryOutcome::NoOp => {
                summary.push_str("- No code changes were made\n");
            }
            DeliveryOutcome::AnalysisOnly => {
                summary.push_str("- Analysis complete, no code changes\n");
            }
            DeliveryOutcome::None => {
                summary.push_str("- Check verification status\n");
            }
        }

        Some(summary.trim_end_matches('\n').to_string())
    }

    fn summarize_failure(output: &str) -> String {
        let lines: Vec<&str> = output.lines().collect();
        if lines.is_empty() {
            return String::new();
        }
        let key_phrases = [
            "error:",
            "failed:",
            "panicked",
            "error\0",
            "Syntax error",
            "cannot find",
            "no such file",
            "undefined reference",
        ];
        for line in lines.iter().take(10) {
            let lower = line.to_lowercase();
            for phrase in &key_phrases {
                if lower.contains(phrase) {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && trimmed.len() < 200 {
                        return trimmed.to_string();
                    }
                }
            }
        }
        let first_line = lines.first().map(|s| s.trim()).unwrap_or("");
        if first_line.len() < 200 {
            first_line.to_string()
        } else {
            format!("{}...", &first_line[..200])
        }
    }

    fn verification_summary(commands: &[VerificationCommand]) -> String {
        if commands.is_empty() {
            return String::new();
        }
        let total = commands.len();
        let failed = commands.iter().filter(|c| c.exit_code != 0).count();
        let last_passed = commands.last().is_some_and(|c| c.exit_code == 0);

        if failed == 0 {
            if total == 1 {
                "\nVerification passed.\n".to_string()
            } else {
                format!("\nAll {} verification commands passed.\n", total)
            }
        } else if last_passed {
            format!(
                "\nFinal verification passed after {} failed attempt{}.\n",
                failed,
                if failed == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "\nVerification failed ({} of {} attempt{} failed).\n",
                failed,
                total,
                if total == 1 { "" } else { "s" }
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{InfluenceMode, SourceKind, SourceLabel};

    #[test]
    fn test_task_result_no_evidence_returns_summary() {
        let result = TaskResult::new("Task completed".to_string());
        let proof = result.format_proof_of_work();
        assert_eq!(proof, "Task completed");
    }

    #[test]
    fn test_task_result_with_files_changed() {
        let result = TaskResult::new("File edited".to_string())
            .with_files_changed(vec!["src/main.rs".to_string()]);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Files Changed"));
        assert!(proof.contains("src/main.rs"));
    }

    #[test]
    fn test_task_result_with_verification() {
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "test result: ok".to_string(),
            exit_code: 0,
            succeeded: true,
        };
        let result = TaskResult::new("Tests passed".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Verification"));
        assert!(proof.contains("PASS"));
    }

    #[test]
    fn test_task_result_files_changed_no_verification_shows_not_performed() {
        let result = TaskResult::new("Files updated".to_string())
            .with_files_changed(vec!["src/main.rs".to_string()]);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("### Verification"));
        assert!(proof.contains("Not performed"));
    }

    #[test]
    fn test_task_result_with_failed_verification() {
        let cmd = VerificationCommand {
            command: "cargo build".to_string(),
            output: "error: failed".to_string(),
            exit_code: 1,
            succeeded: false,
        };
        let result = TaskResult::new("Build failed".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("FAIL"));
    }

    #[test]
    fn test_verification_failure_shows_summary() {
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "test result: FAILED\nerror: file not found".to_string(),
            exit_code: 1,
            succeeded: false,
        };
        let result = TaskResult::new("Tests failed".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("FAIL"));
        assert!(proof.contains("error: file not found"));
    }

    #[test]
    fn test_analysis_only_returns_summary() {
        let result = TaskResult::new("Analysis complete".to_string());
        let proof = result.format_proof_of_work();
        assert_eq!(proof, "Analysis complete");
    }

    #[test]
    fn test_task_result_with_unresolved() {
        let result = TaskResult::new("Partial completion".to_string())
            .with_unresolved_issue("Need to fix edge case".to_string());
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Unresolved"));
        assert!(proof.contains("Need to fix edge case"));
    }

    #[test]
    fn test_task_result_full_proof() {
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "all tests pass".to_string(),
            exit_code: 0,
            succeeded: true,
        };
        let result = TaskResult::new("Implementation complete".to_string())
            .with_files_changed(vec!["src/lib.rs".to_string()])
            .with_verification_command(cmd)
            .with_unresolved_issue("Documentation not updated".to_string());
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Files Changed"));
        assert!(proof.contains("Verification"));
        assert!(proof.contains("Unresolved"));
    }

    #[test]
    fn test_task_result_with_workspace_warnings() {
        let result = TaskResult::new("Task completed".to_string())
            .with_workspace_warnings(vec!["broken_tool: missing script.sh".to_string()]);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Workspace Warnings"));
        assert!(proof.contains("broken_tool: missing script.sh"));
    }

    #[test]
    fn test_task_result_tool_trace_does_not_change_proof_format() {
        let baseline = TaskResult::new("Task completed".to_string()).format_proof_of_work();
        let result = TaskResult::new("Task completed".to_string()).with_tool_trace(vec![
            ToolTraceStep {
                tool_name: "read".to_string(),
                summary: "read README.md".to_string(),
            },
            ToolTraceStep {
                tool_name: "bash".to_string(),
                summary: "verification: cargo test -p topagent-cli".to_string(),
            },
        ]);

        let proof = result.format_proof_of_work();
        assert_eq!(proof, baseline);
    }

    #[test]
    fn test_final_verification_passed_uses_latest_command() {
        let result = TaskResult::new("Done".to_string())
            .with_verification_command(VerificationCommand {
                command: "cargo test".to_string(),
                output: "fail".to_string(),
                exit_code: 1,
                succeeded: false,
            })
            .with_verification_command(VerificationCommand {
                command: "cargo test".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            });

        assert!(result.final_verification_passed());
        assert_eq!(
            result
                .latest_verification_command()
                .map(|command| command.command.as_str()),
            Some("cargo test")
        );
    }

    #[test]
    fn test_final_verification_passed_requires_a_command() {
        let result =
            TaskResult::new("Done".to_string()).with_files_changed(vec!["src/lib.rs".to_string()]);

        assert!(!result.final_verification_passed());
    }

    // ── Regression: exit_code is the source of truth for labels ──

    #[test]
    fn test_exit_code_zero_always_renders_pass() {
        // Regression: even if `succeeded` is somehow false, exit_code 0 must render PASS.
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "Finished successfully\nexit status: 0".to_string(),
            exit_code: 0,
            succeeded: false, // deliberately wrong — rendering must use exit_code
        };
        let result = TaskResult::new("Done".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(
            proof.contains("PASS"),
            "exit 0 must be PASS, got: {}",
            proof
        );
        assert!(
            !proof.contains("FAIL"),
            "exit 0 must not contain FAIL, got: {}",
            proof
        );
    }

    #[test]
    fn test_nonzero_exit_always_renders_fail() {
        let cmd = VerificationCommand {
            command: "cargo build".to_string(),
            output: "error[E0001]: something".to_string(),
            exit_code: 1,
            succeeded: true, // deliberately wrong — rendering must use exit_code
        };
        let result = TaskResult::new("Done".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(
            proof.contains("FAIL"),
            "exit 1 must be FAIL, got: {}",
            proof
        );
        assert!(
            !proof.contains("(PASS)"),
            "exit 1 must not contain PASS label, got: {}",
            proof
        );
    }

    // ── Verification summary aggregation ──

    #[test]
    fn test_single_success_summary() {
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "ok".to_string(),
            exit_code: 0,
            succeeded: true,
        };
        let result = TaskResult::new("Done".to_string()).with_verification_command(cmd);
        let proof = result.format_proof_of_work();
        assert!(
            proof.contains("Verification passed."),
            "single success should say passed, got: {}",
            proof
        );
    }

    #[test]
    fn test_failure_then_success_summary() {
        let fail = VerificationCommand {
            command: "cargo test".to_string(),
            output: "error".to_string(),
            exit_code: 1,
            succeeded: false,
        };
        let pass = VerificationCommand {
            command: "cargo test".to_string(),
            output: "ok".to_string(),
            exit_code: 0,
            succeeded: true,
        };
        let result = TaskResult::new("Done".to_string())
            .with_verification_command(fail)
            .with_verification_command(pass);
        let proof = result.format_proof_of_work();
        assert!(
            proof.contains("Final verification passed after 1 failed attempt."),
            "should note recovery, got: {}",
            proof
        );
    }

    #[test]
    fn test_all_failures_summary() {
        let fail1 = VerificationCommand {
            command: "cargo test".to_string(),
            output: "error".to_string(),
            exit_code: 1,
            succeeded: false,
        };
        let fail2 = VerificationCommand {
            command: "cargo test".to_string(),
            output: "error again".to_string(),
            exit_code: 1,
            succeeded: false,
        };
        let result = TaskResult::new("Done".to_string())
            .with_verification_command(fail1)
            .with_verification_command(fail2);
        let proof = result.format_proof_of_work();
        assert!(
            proof.contains("Verification failed (2 of 2 attempts failed)."),
            "should report all failed, got: {}",
            proof
        );
    }

    #[test]
    fn test_no_verification_has_no_summary() {
        let result =
            TaskResult::new("Done".to_string()).with_files_changed(vec!["f.rs".to_string()]);
        let proof = result.format_proof_of_work();
        assert!(
            !proof.contains("Verification passed"),
            "no verification should not claim passed, got: {}",
            proof
        );
    }

    #[test]
    fn test_format_delivery_summary_emits_structured_shape_for_analysis_with_verification() {
        // No files changed, but verification was attempted (e.g., a "run cargo
        // test" task). The summary must still be the structured shape so the
        // operator and downstream parsers see one consistent format, not a
        // bare string for this branch.
        let cmd = VerificationCommand {
            command: "cargo test".to_string(),
            output: "ok".to_string(),
            exit_code: 0,
            succeeded: true,
        };
        let result = TaskResult::new("Ran test suite".to_string())
            .with_task_mode(crate::plan::TaskMode::PlanAndExecute)
            .with_verification_command(cmd)
            .with_delivery_outcome(DeliveryOutcome::AnalysisOnly);

        let summary = result.format_delivery_summary().expect("summary expected");

        assert!(summary.contains("## Delivery Summary"));
        assert!(summary.contains("### What Changed"));
        assert!(summary.contains("### Files Touched"));
        assert!(summary.contains("(none)"));
        assert!(summary.contains("### Verification Status"));
        assert!(summary.contains("`cargo test`"));
        assert!(summary.contains("PASS"));
        assert!(summary.contains("Analysis complete, no code changes"));
        assert!(
            !summary.contains("Analysis/verification run - no files changed"),
            "must not emit the legacy unstructured bare string"
        );
    }

    #[test]
    fn test_format_delivery_summary_returns_none_for_pure_noop_runs() {
        let result = TaskResult::new("Nothing to do".to_string())
            .with_task_mode(crate::plan::TaskMode::PlanAndExecute)
            .with_delivery_outcome(DeliveryOutcome::NoOp);
        assert!(result.format_delivery_summary().is_none());
    }

    #[test]
    fn test_low_trust_sources_render_in_proof_of_work() {
        let result = TaskResult::new("Done".to_string())
            .with_files_changed(vec!["f.rs".to_string()])
            .with_source_labels(vec![SourceLabel::low(
                SourceKind::FetchedWebContent,
                InfluenceMode::MayDriveAction,
                "curl https://example.com/install.sh",
            )]);
        let proof = result.format_proof_of_work();
        assert!(proof.contains("Trust Notes"));
        assert!(proof.contains("fetched web content"));
    }
}

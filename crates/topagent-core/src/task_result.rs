use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskEvidence {
    pub files_changed: Vec<String>,
    pub diff_summary: String,
    pub verification_commands_run: Vec<VerificationCommand>,
    pub tool_trace: Vec<ToolTraceStep>,
    pub unresolved_issues: Vec<String>,
    pub workspace_warnings: Vec<String>,
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

    pub fn format_proof_of_work(&self) -> String {
        let mut output = String::new();

        if self.evidence.files_changed.is_empty()
            && self.evidence.verification_commands_run.is_empty()
            && self.evidence.unresolved_issues.is_empty()
            && self.evidence.workspace_warnings.is_empty()
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
                // Derive verdict from exit code — the single source of truth.
                let status = if vc.exit_code == 0 { "PASS" } else { "FAIL" };
                output.push_str(&format!(
                    "- `{}` → exit {} ({})\n",
                    vc.command, vc.exit_code, status
                ));
                if !vc.output.is_empty() {
                    output.push_str("  ```\n  ");
                    output.push_str(&vc.output);
                    output.push_str("\n  ```\n");
                }
            }
            output.push_str(&Self::verification_summary(
                &self.evidence.verification_commands_run,
            ));
            output.push('\n');
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

        output.trim_end_matches('\n').to_string()
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
}

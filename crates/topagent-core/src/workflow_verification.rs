use crate::behavior::{BashCommandClass, BehaviorContract};
use crate::plan::TaskQueueStatus;
use crate::task_result::{
    TaskResult, ToolActionOutcome, ToolActionReceipt, VerificationCommand, WorkflowVerification,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowRequirement {
    AnalysisOnly,
    Patch,
    Test,
    Audit,
    CommitReview,
    ReleaseGate,
}

impl WorkflowRequirement {
    fn label(self) -> &'static str {
        match self {
            Self::AnalysisOnly => "analysis_only",
            Self::Patch => "patch",
            Self::Test => "test",
            Self::Audit => "audit",
            Self::CommitReview => "commit_review",
            Self::ReleaseGate => "release_gate",
        }
    }

    fn requires_relevant_verification_pass(self, files_changed: bool) -> bool {
        match self {
            Self::Patch => files_changed,
            Self::Test | Self::ReleaseGate => true,
            Self::AnalysisOnly | Self::Audit | Self::CommitReview => false,
        }
    }
}

pub(crate) struct WorkflowEvidenceEvaluation {
    pub(crate) verification: WorkflowVerification,
    pub(crate) unrecovered_receipt_issues: Vec<String>,
}

pub(crate) fn evaluate_workflow_verification(
    queue: TaskQueueStatus,
    task_result: &TaskResult,
) -> WorkflowEvidenceEvaluation {
    let requirements = workflow_requirements(queue, task_result.has_files_changed());
    let verification_command_count = task_result.verification_commands().len();
    let final_verification_passed = task_result.final_verification_passed();
    let failed_verification_count = task_result
        .verification_commands()
        .iter()
        .filter(|command| !command.succeeded)
        .count();
    let final_relevant_verification_passed =
        latest_relevant_verification_passed(&requirements, task_result.verification_commands());
    let required_verification_present = requirements
        .iter()
        .all(|requirement| requirement_evidence_present(*requirement, task_result));
    let relevant_pass_required = requirements.iter().any(|requirement| {
        requirement.requires_relevant_verification_pass(task_result.has_files_changed())
    });
    let requirement_satisfied = required_verification_present
        && (!relevant_pass_required || final_relevant_verification_passed);
    let unrecovered_receipt_issues = unrecovered_receipt_issues(task_result.tool_receipts());
    let satisfied =
        queue.is_complete() && requirement_satisfied && unrecovered_receipt_issues.is_empty();
    let summary = workflow_verification_summary(
        queue,
        &requirements,
        WorkflowVerificationSummaryInput {
            required_verification_present,
            failed_verification_count,
            final_relevant_verification_passed,
            relevant_pass_required,
            satisfied,
            unrecovered_receipt_issue_count: unrecovered_receipt_issues.len(),
        },
    );

    WorkflowEvidenceEvaluation {
        verification: WorkflowVerification {
            queue,
            verification_command_count,
            final_verification_passed,
            required_verification_present,
            failed_verification_count,
            final_relevant_verification_passed,
            satisfied,
            summary,
        },
        unrecovered_receipt_issues,
    }
}

fn workflow_requirements(queue: TaskQueueStatus, files_changed: bool) -> Vec<WorkflowRequirement> {
    let workflow = queue.workflow;
    let mut requirements = Vec::new();

    if workflow.audit > 0 {
        requirements.push(WorkflowRequirement::Audit);
    }
    if workflow.patch > 0 {
        requirements.push(WorkflowRequirement::Patch);
    }
    if workflow.test > 0 {
        requirements.push(WorkflowRequirement::Test);
    }
    if workflow.commit_review > 0 {
        requirements.push(WorkflowRequirement::CommitReview);
    }
    if workflow.release_gate > 0 {
        requirements.push(WorkflowRequirement::ReleaseGate);
    }

    if requirements.is_empty() {
        if files_changed {
            requirements.push(WorkflowRequirement::Patch);
        } else {
            requirements.push(WorkflowRequirement::AnalysisOnly);
        }
    }

    requirements
}

fn requirement_evidence_present(
    requirement: WorkflowRequirement,
    task_result: &TaskResult,
) -> bool {
    match requirement {
        WorkflowRequirement::AnalysisOnly => true,
        WorkflowRequirement::Patch => {
            !task_result.has_files_changed()
                || has_relevant_verification_evidence(requirement, task_result)
        }
        WorkflowRequirement::Test | WorkflowRequirement::ReleaseGate => {
            has_relevant_verification_evidence(requirement, task_result)
        }
        WorkflowRequirement::Audit => task_result
            .tool_receipts()
            .iter()
            .any(is_successful_local_inspection_receipt),
        WorkflowRequirement::CommitReview => task_result
            .tool_receipts()
            .iter()
            .any(is_successful_commit_review_receipt),
    }
}

fn has_relevant_verification_evidence(
    requirement: WorkflowRequirement,
    task_result: &TaskResult,
) -> bool {
    task_result
        .verification_commands()
        .iter()
        .any(|command| verification_command_matches_requirement(requirement, &command.command))
        || task_result
            .tool_receipts()
            .iter()
            .any(|receipt| verification_receipt_matches_requirement(requirement, receipt))
}

fn latest_relevant_verification_passed(
    requirements: &[WorkflowRequirement],
    commands: &[VerificationCommand],
) -> bool {
    commands
        .iter()
        .rev()
        .find(|command| {
            requirements.iter().any(|requirement| {
                verification_command_matches_requirement(*requirement, &command.command)
            })
        })
        .is_some_and(|command| command.succeeded)
}

fn verification_command_matches_requirement(
    requirement: WorkflowRequirement,
    command: &str,
) -> bool {
    match requirement {
        WorkflowRequirement::Patch | WorkflowRequirement::Test => true,
        WorkflowRequirement::ReleaseGate => is_release_gate_verification_command(command),
        WorkflowRequirement::AnalysisOnly
        | WorkflowRequirement::Audit
        | WorkflowRequirement::CommitReview => false,
    }
}

fn verification_receipt_matches_requirement(
    requirement: WorkflowRequirement,
    receipt: &ToolActionReceipt,
) -> bool {
    if !is_successful_receipt(receipt) || receipt.tool_name != "bash" {
        return false;
    }
    let Some(command) = bash_command_from_receipt(receipt) else {
        return false;
    };
    match requirement {
        WorkflowRequirement::Patch | WorkflowRequirement::Test => {
            matches!(
                classify_bash_command(command),
                BashCommandClass::Verification
            )
        }
        WorkflowRequirement::ReleaseGate => is_release_gate_verification_command(command),
        WorkflowRequirement::AnalysisOnly
        | WorkflowRequirement::Audit
        | WorkflowRequirement::CommitReview => false,
    }
}

fn is_release_gate_verification_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "scripts/ci-gate.sh",
        "./scripts/ci-gate.sh",
        "cargo test",
        "cargo clippy",
        "cargo build",
        "cargo fmt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_successful_local_inspection_receipt(receipt: &ToolActionReceipt) -> bool {
    if !is_successful_receipt(receipt) {
        return false;
    }

    match receipt.tool_name.as_str() {
        "read" | "rg" | "git_diff" | "git_status" | "git_branch" => true,
        "bash" => bash_command_from_receipt(receipt).is_some_and(is_inspection_bash_command),
        _ => false,
    }
}

fn is_successful_commit_review_receipt(receipt: &ToolActionReceipt) -> bool {
    if !is_successful_receipt(receipt) {
        return false;
    }

    match receipt.tool_name.as_str() {
        "git_diff" | "git_status" => true,
        "bash" => bash_command_from_receipt(receipt).is_some_and(is_commit_review_bash_command),
        _ => false,
    }
}

fn is_successful_receipt(receipt: &ToolActionReceipt) -> bool {
    receipt.admitted && receipt.outcome == ToolActionOutcome::Succeeded
}

fn bash_command_from_receipt(receipt: &ToolActionReceipt) -> Option<&str> {
    receipt.summary.strip_prefix("bash: ").map(str::trim)
}

fn is_inspection_bash_command(command: &str) -> bool {
    if classify_bash_command(command) != BashCommandClass::ResearchSafe {
        return false;
    }
    let lower = command.trim().to_ascii_lowercase();
    [
        "pwd",
        "ls",
        "rg",
        "find",
        "grep",
        "cat",
        "head",
        "tail",
        "wc",
        "git status",
        "git diff",
        "git log",
        "git show",
        "git branch",
    ]
    .iter()
    .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} ")))
}

fn is_commit_review_bash_command(command: &str) -> bool {
    let lower = command.trim().to_ascii_lowercase();
    ["git diff", "git log", "git show", "git status"]
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} ")))
}

fn unrecovered_receipt_issues(receipts: &[ToolActionReceipt]) -> Vec<String> {
    receipts
        .iter()
        .enumerate()
        .filter(|(index, receipt)| {
            receipt.outcome != ToolActionOutcome::Succeeded
                && !has_later_recovery_receipt(receipts, *index, receipt)
        })
        .map(|(_, receipt)| {
            format!(
                "Unrecovered tool {}: {} - {}",
                receipt.outcome.label(),
                receipt.tool_name,
                receipt.summary
            )
        })
        .collect()
}

fn has_later_recovery_receipt(
    receipts: &[ToolActionReceipt],
    index: usize,
    receipt: &ToolActionReceipt,
) -> bool {
    receipts.iter().skip(index + 1).any(|later| {
        is_successful_receipt(later)
            && later.tool_name == receipt.tool_name
            && receipt_recovery_key(receipt).is_some_and(|failed_key| {
                receipt_recovery_key(later).is_some_and(|later_key| later_key == failed_key)
            })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReceiptRecoveryKey {
    Path { tool: String, path: String },
    Bash { family: String, verification: bool },
    Summary { tool: String, summary: String },
}

fn receipt_recovery_key(receipt: &ToolActionReceipt) -> Option<ReceiptRecoveryKey> {
    match receipt.tool_name.as_str() {
        "bash" => bash_command_from_receipt(receipt).map(|command| {
            let verification = classify_bash_command(command) == BashCommandClass::Verification;
            ReceiptRecoveryKey::Bash {
                family: bash_command_family(command),
                verification,
            }
        }),
        "read" | "write" | "edit" | "external_send" => {
            receipt_target(&receipt.summary, &receipt.tool_name).map(|path| {
                ReceiptRecoveryKey::Path {
                    tool: receipt.tool_name.clone(),
                    path,
                }
            })
        }
        _ => (!receipt.summary.trim().is_empty()).then(|| ReceiptRecoveryKey::Summary {
            tool: receipt.tool_name.clone(),
            summary: receipt.summary.clone(),
        }),
    }
}

fn receipt_target(summary: &str, tool_name: &str) -> Option<String> {
    let prefix = format!("{tool_name}: ");
    summary
        .strip_prefix(&prefix)
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(ToOwned::to_owned)
}

fn bash_command_family(command: &str) -> String {
    let normalized = command.trim().to_ascii_lowercase();
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        [] => String::new(),
        ["./scripts/ci-gate.sh" | "scripts/ci-gate.sh", ..] => "scripts/ci-gate.sh".to_string(),
        ["cargo", subcommand, ..] => format!("cargo {subcommand}"),
        ["npm", "run", subcommand, ..] => format!("npm run {subcommand}"),
        ["npm", subcommand, ..] => format!("npm {subcommand}"),
        ["pnpm", subcommand, ..] => format!("pnpm {subcommand}"),
        ["yarn", subcommand, ..] => format!("yarn {subcommand}"),
        ["go", subcommand, ..] => format!("go {subcommand}"),
        ["make", target, ..] => format!("make {target}"),
        ["git", subcommand, ..] => format!("git {subcommand}"),
        [command, ..] => (*command).to_string(),
    }
}

fn classify_bash_command(command: &str) -> BashCommandClass {
    BehaviorContract::default().classify_bash_command(command)
}

struct WorkflowVerificationSummaryInput {
    required_verification_present: bool,
    failed_verification_count: usize,
    final_relevant_verification_passed: bool,
    relevant_pass_required: bool,
    satisfied: bool,
    unrecovered_receipt_issue_count: usize,
}

fn workflow_verification_summary(
    queue: TaskQueueStatus,
    requirements: &[WorkflowRequirement],
    input: WorkflowVerificationSummaryInput,
) -> String {
    let requirement_list = requirements
        .iter()
        .map(|requirement| requirement.label())
        .collect::<Vec<_>>()
        .join(", ");

    if input.satisfied {
        let mut summary = format!(
            "plan complete ({}/{} done) and workflow evidence satisfied ({requirement_list})",
            queue.done, queue.total,
        );
        if input.failed_verification_count > 0 {
            summary.push_str(&format!(
                "; {} failed verification attempt(s) recovered by final relevant pass",
                input.failed_verification_count
            ));
        }
        return summary;
    }

    let mut parts = Vec::new();
    if !queue.is_complete() {
        parts.push(format!(
            "plan incomplete: {}/{} done, {} pending, {} active, {} blocked",
            queue.done, queue.total, queue.pending, queue.in_progress, queue.blocked
        ));
    }
    if !input.required_verification_present {
        parts.push(format!("required evidence missing: {requirement_list}"));
    }
    if input.relevant_pass_required && !input.final_relevant_verification_passed {
        parts.push("final relevant verification did not pass".to_string());
    }
    if input.failed_verification_count > 0 && input.final_relevant_verification_passed {
        parts.push(format!(
            "{} failed verification attempt(s) recovered by final relevant pass",
            input.failed_verification_count
        ));
    }
    if input.unrecovered_receipt_issue_count > 0 {
        parts.push(format!(
            "{} unrecovered blocked/failed tool receipt(s)",
            input.unrecovered_receipt_issue_count
        ));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{CodingWorkflowStatus, TaskQueueStatus};

    fn complete_queue(workflow: CodingWorkflowStatus) -> TaskQueueStatus {
        TaskQueueStatus {
            total: 1,
            done: 1,
            workflow,
            ..TaskQueueStatus::default()
        }
    }

    fn workflow_with(kind: &str) -> CodingWorkflowStatus {
        let mut workflow = CodingWorkflowStatus::default();
        match kind {
            "audit" => workflow.audit = 1,
            "patch" => workflow.patch = 1,
            "test" => workflow.test = 1,
            "commit_review" => workflow.commit_review = 1,
            "release_gate" => workflow.release_gate = 1,
            _ => workflow.uncategorized = 1,
        }
        workflow
    }

    fn receipt(tool: &str, outcome: ToolActionOutcome, summary: &str) -> ToolActionReceipt {
        ToolActionReceipt::new(
            tool,
            "patch",
            outcome == ToolActionOutcome::Succeeded,
            outcome,
            summary,
        )
    }

    fn bash_receipt(outcome: ToolActionOutcome, command: &str) -> ToolActionReceipt {
        receipt("bash", outcome, &format!("bash: {command}"))
    }

    #[test]
    fn failed_write_to_one_path_is_not_recovered_by_successful_write_to_another() {
        let issues = unrecovered_receipt_issues(&[
            receipt("write", ToolActionOutcome::Failed, "write: src/lib.rs"),
            receipt("write", ToolActionOutcome::Succeeded, "write: README.md"),
        ]);

        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("src/lib.rs"));
    }

    #[test]
    fn failed_cargo_test_is_recovered_by_later_successful_cargo_test() {
        let issues = unrecovered_receipt_issues(&[
            bash_receipt(ToolActionOutcome::Failed, "cargo test --quiet"),
            bash_receipt(ToolActionOutcome::Succeeded, "cargo test --quiet"),
        ]);

        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn failed_cargo_test_is_not_recovered_by_successful_pwd() {
        let issues = unrecovered_receipt_issues(&[
            bash_receipt(ToolActionOutcome::Failed, "cargo test --quiet"),
            bash_receipt(ToolActionOutcome::Succeeded, "pwd"),
        ]);

        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("cargo test"));
    }

    #[test]
    fn blocked_external_send_is_not_recovered_by_unrelated_successful_tool() {
        let issues = unrecovered_receipt_issues(&[
            receipt(
                "external_send",
                ToolActionOutcome::Blocked,
                "external_send: https://example.invalid/upload",
            ),
            receipt("read", ToolActionOutcome::Succeeded, "read: README.md"),
        ]);

        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("external_send"));
    }

    #[test]
    fn audit_with_only_web_search_evidence_does_not_satisfy_local_repo_audit() {
        let result = TaskResult::new("done".to_string()).with_tool_receipt(receipt(
            "web_search",
            ToolActionOutcome::Succeeded,
            "web_search",
        ));
        let evaluation =
            evaluate_workflow_verification(complete_queue(workflow_with("audit")), &result);

        assert!(!evaluation.verification.satisfied);
        assert!(!evaluation.verification.required_verification_present);
    }

    #[test]
    fn audit_with_local_inspection_receipts_satisfies() {
        for (tool, summary) in [
            ("read", "read: src/lib.rs"),
            ("rg", "rg: fn main"),
            ("git_diff", "git_diff"),
        ] {
            let result = TaskResult::new("done".to_string()).with_tool_receipt(receipt(
                tool,
                ToolActionOutcome::Succeeded,
                summary,
            ));
            let evaluation =
                evaluate_workflow_verification(complete_queue(workflow_with("audit")), &result);

            assert!(
                evaluation.verification.satisfied,
                "{tool} should satisfy local audit evidence: {}",
                evaluation.verification.summary
            );
        }
    }
}

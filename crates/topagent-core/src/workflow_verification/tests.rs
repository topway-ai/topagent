use super::*;
use crate::plan::{CodingWorkflowStatus, TaskQueueStatus};
use crate::task_result::{TaskResult, ToolActionOutcome, ToolActionReceipt, VerificationCommand};

fn complete_queue(workflow: CodingWorkflowStatus) -> TaskQueueStatus {
    TaskQueueStatus {
        total: 1,
        done: 1,
        workflow,
        ..TaskQueueStatus::default()
    }
}

fn incomplete_queue(workflow: CodingWorkflowStatus) -> TaskQueueStatus {
    TaskQueueStatus {
        total: 2,
        done: 1,
        pending: 1,
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

fn verification(command: &str, exit_code: i32) -> VerificationCommand {
    VerificationCommand {
        command: command.to_string(),
        output: String::new(),
        exit_code,
        succeeded: exit_code == 0,
    }
}

#[test]
fn analysis_only_with_no_file_changes_can_satisfy_when_queue_complete() {
    let result = TaskResult::new("analysis".to_string());
    let evaluation =
        evaluate_workflow_verification(complete_queue(CodingWorkflowStatus::default()), &result);

    assert!(
        evaluation.verification.satisfied,
        "{}",
        evaluation.verification.summary
    );
    assert!(evaluation.unresolved_evidence_gaps.is_empty());
}

#[test]
fn analysis_only_does_not_hide_unrecovered_receipt_issue() {
    let result = TaskResult::new("analysis".to_string()).with_tool_receipt(receipt(
        "read",
        ToolActionOutcome::Failed,
        "read: src/lib.rs",
    ));
    let evaluation =
        evaluate_workflow_verification(complete_queue(CodingWorkflowStatus::default()), &result);

    assert!(!evaluation.verification.satisfied);
    assert_eq!(evaluation.unrecovered_receipt_issues.len(), 1);
}

#[test]
fn analysis_only_requires_complete_queue() {
    let result = TaskResult::new("analysis".to_string());
    let evaluation =
        evaluate_workflow_verification(incomplete_queue(CodingWorkflowStatus::default()), &result);

    assert!(!evaluation.verification.satisfied);
    assert!(evaluation.verification.summary.contains("plan incomplete"));
}

#[test]
fn failed_write_to_one_path_is_not_recovered_by_successful_write_to_another() {
    let result = TaskResult::new("done".to_string()).with_tool_receipts(vec![
        receipt("write", ToolActionOutcome::Failed, "write: src/lib.rs"),
        receipt("write", ToolActionOutcome::Succeeded, "write: README.md"),
    ]);
    let evaluation =
        evaluate_workflow_verification(complete_queue(CodingWorkflowStatus::default()), &result);

    assert_eq!(evaluation.unrecovered_receipt_issues.len(), 1);
    assert!(evaluation.unrecovered_receipt_issues[0].contains("src/lib.rs"));
}

#[test]
fn failed_cargo_test_is_recovered_by_later_successful_cargo_test_but_count_remains() {
    let result = TaskResult::new("done".to_string())
        .with_files_changed(vec!["src/lib.rs".to_string()])
        .with_verification_command(verification("cargo test --quiet", 1))
        .with_verification_command(verification("cargo test --quiet", 0))
        .with_tool_receipts(vec![
            bash_receipt(ToolActionOutcome::Failed, "cargo test --quiet"),
            bash_receipt(ToolActionOutcome::Succeeded, "cargo test --quiet"),
        ]);
    let evaluation =
        evaluate_workflow_verification(complete_queue(workflow_with("patch")), &result);

    assert!(evaluation.unrecovered_receipt_issues.is_empty());
    assert!(
        evaluation.verification.satisfied,
        "{}",
        evaluation.verification.summary
    );
    assert_eq!(evaluation.verification.failed_verification_count, 1);
}

#[test]
fn failed_cargo_test_is_not_recovered_by_successful_pwd() {
    let result = TaskResult::new("done".to_string()).with_tool_receipts(vec![
        bash_receipt(ToolActionOutcome::Failed, "cargo test --quiet"),
        bash_receipt(ToolActionOutcome::Succeeded, "pwd"),
    ]);
    let evaluation =
        evaluate_workflow_verification(complete_queue(CodingWorkflowStatus::default()), &result);

    assert_eq!(evaluation.unrecovered_receipt_issues.len(), 1);
    assert!(evaluation.unrecovered_receipt_issues[0].contains("cargo test"));
}

#[test]
fn blocked_external_send_is_not_recovered_by_unrelated_successful_read() {
    let result = TaskResult::new("done".to_string()).with_tool_receipts(vec![
        receipt(
            "external_send",
            ToolActionOutcome::Blocked,
            "external_send: https://example.invalid/upload",
        ),
        receipt("read", ToolActionOutcome::Succeeded, "read: README.md"),
    ]);
    let evaluation =
        evaluate_workflow_verification(complete_queue(CodingWorkflowStatus::default()), &result);

    assert_eq!(evaluation.unrecovered_receipt_issues.len(), 1);
    assert!(evaluation.unrecovered_receipt_issues[0].contains("external_send"));
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
        ("bash", "bash: rg \"fn main\" src"),
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

#[test]
fn commit_review_requires_git_diff_log_show_or_status_evidence() {
    let only_read = TaskResult::new("done".to_string()).with_tool_receipt(receipt(
        "read",
        ToolActionOutcome::Succeeded,
        "read: src/lib.rs",
    ));
    let rejected =
        evaluate_workflow_verification(complete_queue(workflow_with("commit_review")), &only_read);
    assert!(!rejected.verification.satisfied);

    for summary in ["git_diff", "bash: git log -1", "bash: git show --stat"] {
        let result = TaskResult::new("done".to_string()).with_tool_receipt(receipt(
            if summary == "git_diff" {
                "git_diff"
            } else {
                "bash"
            },
            ToolActionOutcome::Succeeded,
            summary,
        ));
        let accepted =
            evaluate_workflow_verification(complete_queue(workflow_with("commit_review")), &result);
        assert!(accepted.verification.satisfied, "{summary}");
    }
}

#[test]
fn release_gate_requires_release_gate_or_equivalent_verification() {
    let pwd = TaskResult::new("done".to_string())
        .with_verification_command(verification("pwd", 0))
        .with_tool_receipt(bash_receipt(ToolActionOutcome::Succeeded, "pwd"));
    let rejected =
        evaluate_workflow_verification(complete_queue(workflow_with("release_gate")), &pwd);
    assert!(!rejected.verification.satisfied);

    let ci_gate = TaskResult::new("done".to_string())
        .with_verification_command(verification("scripts/ci-gate.sh", 0))
        .with_tool_receipt(bash_receipt(
            ToolActionOutcome::Succeeded,
            "scripts/ci-gate.sh",
        ));
    let accepted =
        evaluate_workflow_verification(complete_queue(workflow_with("release_gate")), &ci_gate);
    assert!(
        accepted.verification.satisfied,
        "{}",
        accepted.verification.summary
    );
}

#[test]
fn patch_with_changed_files_requires_relevant_final_verification_pass() {
    let result = TaskResult::new("done".to_string())
        .with_files_changed(vec!["src/lib.rs".to_string()])
        .with_verification_command(verification("cargo test", 1));
    let evaluation =
        evaluate_workflow_verification(complete_queue(workflow_with("patch")), &result);

    assert!(!evaluation.verification.satisfied);
    assert!(evaluation.verification.required_verification_present);
    assert!(!evaluation.verification.final_relevant_verification_passed);
}

#[test]
fn test_workflow_requires_meaningful_verification_even_without_file_changes() {
    let no_verification = TaskResult::new("done".to_string());
    let evaluation =
        evaluate_workflow_verification(complete_queue(workflow_with("test")), &no_verification);
    assert!(!evaluation.verification.satisfied);
    assert!(!evaluation.verification.required_verification_present);

    let passing = TaskResult::new("done".to_string())
        .with_verification_command(verification("cargo test --quiet", 0));
    let evaluation =
        evaluate_workflow_verification(complete_queue(workflow_with("test")), &passing);
    assert!(
        evaluation.verification.satisfied,
        "{}",
        evaluation.verification.summary
    );
}

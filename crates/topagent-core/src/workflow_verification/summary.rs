use super::requirements::WorkflowRequirement;
use crate::plan::TaskQueueStatus;

pub(crate) struct WorkflowVerificationSummaryInput {
    pub(crate) required_verification_present: bool,
    pub(crate) failed_verification_count: usize,
    pub(crate) final_relevant_verification_passed: bool,
    pub(crate) relevant_pass_required: bool,
    pub(crate) satisfied: bool,
    pub(crate) unrecovered_receipt_issue_count: usize,
}

pub(crate) fn workflow_verification_summary(
    queue: TaskQueueStatus,
    requirements: &[WorkflowRequirement],
    input: WorkflowVerificationSummaryInput,
) -> String {
    let requirement_list = requirement_list(requirements);

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

pub(crate) fn unresolved_evidence_gaps(
    queue: TaskQueueStatus,
    requirements: &[WorkflowRequirement],
    required_verification_present: bool,
    relevant_pass_required: bool,
    final_relevant_verification_passed: bool,
    unrecovered_receipt_issue_count: usize,
) -> Vec<String> {
    let mut gaps = Vec::new();
    if !queue.is_complete() {
        gaps.push(format!(
            "queue incomplete: {}/{} done, {} pending, {} active, {} blocked",
            queue.done, queue.total, queue.pending, queue.in_progress, queue.blocked
        ));
    }
    if !required_verification_present {
        gaps.push(format!(
            "missing evidence for {}",
            requirement_list(requirements)
        ));
    }
    if relevant_pass_required && !final_relevant_verification_passed {
        gaps.push("missing final relevant passing verification".to_string());
    }
    if unrecovered_receipt_issue_count > 0 {
        gaps.push(format!(
            "{unrecovered_receipt_issue_count} unrecovered blocked/failed receipt(s)"
        ));
    }
    gaps
}

pub(crate) fn next_required_evidence_action(
    queue: TaskQueueStatus,
    requirements: &[WorkflowRequirement],
    required_verification_present: bool,
    relevant_pass_required: bool,
    final_relevant_verification_passed: bool,
    unrecovered_receipt_issue_count: usize,
) -> Option<String> {
    if !queue.is_complete() {
        return Some("finish or explicitly block the remaining plan queue".to_string());
    }
    if !required_verification_present {
        let labels = requirement_list(requirements);
        if requirements
            .iter()
            .any(|req| req.requires_release_gate_command)
        {
            return Some(
                "run scripts/ci-gate.sh or an equivalent fmt/clippy/test/build gate".to_string(),
            );
        }
        if requirements
            .iter()
            .any(|req| req.requires_test_or_verification_command)
        {
            return Some("run a relevant verification command".to_string());
        }
        if requirements.iter().any(|req| req.requires_local_inspection) {
            return Some(format!("collect local repository evidence for {labels}"));
        }
        return Some(format!("collect required evidence for {labels}"));
    }
    if relevant_pass_required && !final_relevant_verification_passed {
        return Some(
            "rerun the relevant verification until the final relevant attempt passes".to_string(),
        );
    }
    if unrecovered_receipt_issue_count > 0 {
        return Some("recover or explicitly report blocked/failed tool attempts".to_string());
    }
    None
}

fn requirement_list(requirements: &[WorkflowRequirement]) -> String {
    requirements
        .iter()
        .map(|requirement| requirement.label())
        .collect::<Vec<_>>()
        .join(", ")
}

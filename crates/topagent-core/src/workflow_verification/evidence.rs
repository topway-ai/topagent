use super::command_match::{
    is_meaningful_verification_command, is_release_gate_verification_command,
};
use super::receipt_recovery::unrecovered_receipt_issues;
use super::requirements::{workflow_requirements, WorkflowRequirement, WorkflowRequirementKind};
use super::summary::{
    next_required_evidence_action, unresolved_evidence_gaps, workflow_verification_summary,
    WorkflowVerificationSummaryInput,
};
use crate::plan::TaskQueueStatus;
use crate::receipt_index::{bash_command_from_receipt, is_successful_receipt, ReceiptIndex};
use crate::task_result::{
    TaskResult, ToolActionReceipt, VerificationCommand, WorkflowVerification,
};

pub(crate) struct WorkflowEvidence<'a> {
    pub(crate) changed_files: &'a [String],
    pub(crate) verification_commands: &'a [VerificationCommand],
    pub(crate) receipts: &'a [ToolActionReceipt],
}

impl<'a> WorkflowEvidence<'a> {
    pub(crate) fn new(
        changed_files: &'a [String],
        verification_commands: &'a [VerificationCommand],
        receipts: &'a [ToolActionReceipt],
    ) -> Self {
        Self {
            changed_files,
            verification_commands,
            receipts,
        }
    }

    pub(crate) fn from_task_result(task_result: &'a TaskResult) -> Self {
        Self::new(
            task_result.files_changed(),
            task_result.verification_commands(),
            task_result.tool_receipts(),
        )
    }

    fn has_files_changed(&self) -> bool {
        !self.changed_files.is_empty()
    }
}

pub(crate) struct WorkflowEvidenceEvaluation {
    pub(crate) verification: WorkflowVerification,
    pub(crate) unrecovered_receipt_issues: Vec<String>,
    pub(crate) unresolved_evidence_gaps: Vec<String>,
    pub(crate) next_required_evidence_action: Option<String>,
}

pub(crate) fn evaluate_workflow_evidence(
    queue: TaskQueueStatus,
    evidence: &WorkflowEvidence<'_>,
) -> WorkflowEvidenceEvaluation {
    let requirements = workflow_requirements(queue, evidence.has_files_changed());
    let verification_command_count = evidence.verification_commands.len();
    let final_verification_passed = evidence
        .verification_commands
        .last()
        .is_some_and(|command| command.succeeded);
    let failed_verification_count = evidence
        .verification_commands
        .iter()
        .filter(|command| !command.succeeded)
        .count();
    let final_relevant_verification_passed =
        latest_relevant_verification_passed(&requirements, evidence);
    let required_verification_present = requirements
        .iter()
        .all(|requirement| requirement_evidence_present(*requirement, evidence));
    let relevant_pass_required = requirements
        .iter()
        .any(|requirement| requirement.final_relevant_pass_required(evidence.has_files_changed()));
    let requirement_satisfied = required_verification_present
        && (!relevant_pass_required || final_relevant_verification_passed);
    let unrecovered_receipt_issues = unrecovered_receipt_issues(evidence.receipts);
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
    let unresolved_evidence_gaps = unresolved_evidence_gaps(
        queue,
        &requirements,
        required_verification_present,
        relevant_pass_required,
        final_relevant_verification_passed,
        unrecovered_receipt_issues.len(),
    );
    let next_required_evidence_action = next_required_evidence_action(
        queue,
        &requirements,
        required_verification_present,
        relevant_pass_required,
        final_relevant_verification_passed,
        unrecovered_receipt_issues.len(),
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
        unresolved_evidence_gaps,
        next_required_evidence_action,
    }
}

fn requirement_evidence_present(
    requirement: WorkflowRequirement,
    evidence: &WorkflowEvidence<'_>,
) -> bool {
    match requirement.kind {
        WorkflowRequirementKind::AnalysisOnly => true,
        WorkflowRequirementKind::Patch => {
            !evidence.has_files_changed()
                || has_relevant_verification_evidence(requirement, evidence)
        }
        WorkflowRequirementKind::Test | WorkflowRequirementKind::ReleaseGate => {
            has_relevant_verification_evidence(requirement, evidence)
        }
        WorkflowRequirementKind::Audit => {
            ReceiptIndex::new(evidence.receipts).has_successful_local_inspection()
        }
        WorkflowRequirementKind::CommitReview => {
            ReceiptIndex::new(evidence.receipts).has_successful_commit_review_inspection()
        }
    }
}

fn has_relevant_verification_evidence(
    requirement: WorkflowRequirement,
    evidence: &WorkflowEvidence<'_>,
) -> bool {
    evidence
        .verification_commands
        .iter()
        .any(|command| verification_command_matches_requirement(requirement, &command.command))
        || evidence
            .receipts
            .iter()
            .any(|receipt| verification_receipt_matches_requirement(requirement, receipt))
}

fn latest_relevant_verification_passed(
    requirements: &[WorkflowRequirement],
    evidence: &WorkflowEvidence<'_>,
) -> bool {
    evidence
        .verification_commands
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
    match requirement.kind {
        WorkflowRequirementKind::Patch | WorkflowRequirementKind::Test => {
            is_meaningful_verification_command(command)
        }
        WorkflowRequirementKind::ReleaseGate => is_release_gate_verification_command(command),
        WorkflowRequirementKind::AnalysisOnly
        | WorkflowRequirementKind::Audit
        | WorkflowRequirementKind::CommitReview => false,
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
    match requirement.kind {
        WorkflowRequirementKind::Patch | WorkflowRequirementKind::Test => {
            is_meaningful_verification_command(command)
        }
        WorkflowRequirementKind::ReleaseGate => is_release_gate_verification_command(command),
        WorkflowRequirementKind::AnalysisOnly
        | WorkflowRequirementKind::Audit
        | WorkflowRequirementKind::CommitReview => false,
    }
}

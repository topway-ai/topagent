mod command_match;
mod evidence;
mod receipt_recovery;
mod requirements;
mod summary;

#[cfg(test)]
mod tests;

pub(crate) use evidence::{WorkflowEvidence, WorkflowEvidenceEvaluation};

use crate::plan::TaskQueueStatus;
use crate::task_result::TaskResult;

pub(crate) fn evaluate_workflow_verification(
    queue: TaskQueueStatus,
    task_result: &TaskResult,
) -> WorkflowEvidenceEvaluation {
    let evidence = WorkflowEvidence::from_task_result(task_result);
    evidence::evaluate_workflow_evidence(queue, &evidence)
}

pub(crate) fn evaluate_workflow_evidence(
    queue: TaskQueueStatus,
    evidence: &WorkflowEvidence<'_>,
) -> WorkflowEvidenceEvaluation {
    evidence::evaluate_workflow_evidence(queue, evidence)
}

use crate::plan::{CodingWorkflowStatus, TaskQueueStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkflowRequirementKind {
    AnalysisOnly,
    Patch,
    Test,
    Audit,
    CommitReview,
    ReleaseGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkflowRequirement {
    pub(crate) kind: WorkflowRequirementKind,
    pub(crate) requires_queue_complete: bool,
    pub(crate) requires_local_inspection: bool,
    pub(crate) requires_test_or_verification_command: bool,
    pub(crate) requires_release_gate_command: bool,
    pub(crate) requires_final_relevant_pass: bool,
    pub(crate) allows_no_file_change_shortcut: bool,
    pub(crate) allows_web_research_as_support_only: bool,
}

impl WorkflowRequirement {
    pub(crate) fn for_kind(kind: WorkflowRequirementKind) -> Self {
        match kind {
            WorkflowRequirementKind::AnalysisOnly => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: false,
                requires_test_or_verification_command: false,
                requires_release_gate_command: false,
                requires_final_relevant_pass: false,
                allows_no_file_change_shortcut: true,
                allows_web_research_as_support_only: true,
            },
            WorkflowRequirementKind::Patch => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: false,
                requires_test_or_verification_command: true,
                requires_release_gate_command: false,
                requires_final_relevant_pass: true,
                allows_no_file_change_shortcut: true,
                allows_web_research_as_support_only: false,
            },
            WorkflowRequirementKind::Test => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: false,
                requires_test_or_verification_command: true,
                requires_release_gate_command: false,
                requires_final_relevant_pass: true,
                allows_no_file_change_shortcut: false,
                allows_web_research_as_support_only: false,
            },
            WorkflowRequirementKind::Audit => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: true,
                requires_test_or_verification_command: false,
                requires_release_gate_command: false,
                requires_final_relevant_pass: false,
                allows_no_file_change_shortcut: false,
                allows_web_research_as_support_only: true,
            },
            WorkflowRequirementKind::CommitReview => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: true,
                requires_test_or_verification_command: false,
                requires_release_gate_command: false,
                requires_final_relevant_pass: false,
                allows_no_file_change_shortcut: false,
                allows_web_research_as_support_only: false,
            },
            WorkflowRequirementKind::ReleaseGate => Self {
                kind,
                requires_queue_complete: true,
                requires_local_inspection: false,
                requires_test_or_verification_command: true,
                requires_release_gate_command: true,
                requires_final_relevant_pass: true,
                allows_no_file_change_shortcut: false,
                allows_web_research_as_support_only: false,
            },
        }
    }

    pub(crate) fn label(self) -> &'static str {
        self.kind.label()
    }

    pub(crate) fn final_relevant_pass_required(self, files_changed: bool) -> bool {
        if self.kind == WorkflowRequirementKind::Patch && !files_changed {
            return false;
        }
        self.requires_final_relevant_pass
    }
}

impl WorkflowRequirementKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AnalysisOnly => "analysis_only",
            Self::Patch => "patch",
            Self::Test => "test",
            Self::Audit => "audit",
            Self::CommitReview => "commit_review",
            Self::ReleaseGate => "release_gate",
        }
    }
}

pub(crate) fn workflow_requirements(
    queue: TaskQueueStatus,
    files_changed: bool,
) -> Vec<WorkflowRequirement> {
    let workflow = queue.workflow;
    let mut requirements = workflow_kinds(workflow)
        .into_iter()
        .map(WorkflowRequirement::for_kind)
        .collect::<Vec<_>>();

    if requirements.is_empty() {
        let kind = if files_changed {
            WorkflowRequirementKind::Patch
        } else {
            WorkflowRequirementKind::AnalysisOnly
        };
        requirements.push(WorkflowRequirement::for_kind(kind));
    }

    requirements
}

fn workflow_kinds(workflow: CodingWorkflowStatus) -> Vec<WorkflowRequirementKind> {
    let mut kinds = Vec::new();
    if workflow.audit > 0 {
        kinds.push(WorkflowRequirementKind::Audit);
    }
    if workflow.patch > 0 {
        kinds.push(WorkflowRequirementKind::Patch);
    }
    if workflow.test > 0 {
        kinds.push(WorkflowRequirementKind::Test);
    }
    if workflow.commit_review > 0 {
        kinds.push(WorkflowRequirementKind::CommitReview);
    }
    if workflow.release_gate > 0 {
        kinds.push(WorkflowRequirementKind::ReleaseGate);
    }
    kinds
}

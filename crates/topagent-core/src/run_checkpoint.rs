use crate::plan::{TaskMode, TaskQueueStatus};
use crate::prompt_budget::MAX_RUN_CHECKPOINT_CHARS;
use crate::receipt_index::ReceiptIndex;
use serde::{Deserialize, Serialize};

const MAX_ANCHORS_PER_FIELD: usize = 8;
const MAX_ANCHOR_CHARS: usize = 120;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunCheckpoint {
    pub objective: Option<String>,
    pub workflow_kinds: Vec<String>,
    pub task_mode: Option<String>,
    pub phase: Option<String>,
    pub queue_status: Option<String>,
    pub active_files: Vec<String>,
    pub changed_files: Vec<String>,
    pub files_inspected: Vec<String>,
    pub known_blockers: Vec<String>,
    pub failed_verification_anchors: Vec<String>,
    pub latest_relevant_verification_status: Option<String>,
    pub unresolved_workflow_evidence_gaps: Vec<String>,
    pub low_trust_influence_notes: Vec<String>,
    pub next_required_evidence_action: Option<String>,
    pub receipt_issue_anchors: Vec<String>,
}

impl RunCheckpoint {
    pub fn render_compact(&self) -> String {
        let mut lines = Vec::new();
        if let Some(objective) = &self.objective {
            lines.push(format!("objective: {}", compact_anchor(objective)));
        }
        if let Some(task_mode) = &self.task_mode {
            lines.push(format!("task mode: {task_mode}"));
        }
        if let Some(phase) = &self.phase {
            lines.push(format!("phase: {phase}"));
        }
        if !self.workflow_kinds.is_empty() {
            lines.push(format!("workflow: {}", self.workflow_kinds.join(", ")));
        }
        if let Some(queue_status) = &self.queue_status {
            lines.push(format!("queue: {queue_status}"));
        }
        push_list(&mut lines, "active files", &self.active_files);
        push_list(&mut lines, "changed files", &self.changed_files);
        push_list(&mut lines, "files inspected", &self.files_inspected);
        push_list(&mut lines, "blockers", &self.known_blockers);
        push_list(
            &mut lines,
            "failed verification",
            &self.failed_verification_anchors,
        );
        if let Some(status) = &self.latest_relevant_verification_status {
            lines.push(format!("latest relevant verification: {status}"));
        }
        push_list(
            &mut lines,
            "unresolved evidence gaps",
            &self.unresolved_workflow_evidence_gaps,
        );
        push_list(
            &mut lines,
            "low-trust influence",
            &self.low_trust_influence_notes,
        );
        push_list(&mut lines, "receipt issues", &self.receipt_issue_anchors);
        if let Some(next) = &self.next_required_evidence_action {
            lines.push(format!("next required evidence/action: {next}"));
        }

        let rendered = lines
            .into_iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        truncate_chars(rendered, MAX_RUN_CHECKPOINT_CHARS)
    }

    pub fn validate_budget(&self) -> Result<(), String> {
        let len = self.render_compact().chars().count();
        if len > MAX_RUN_CHECKPOINT_CHARS {
            return Err(format!(
                "RunCheckpoint rendered to {len} chars, above {MAX_RUN_CHECKPOINT_CHARS}; inspect checkpoint anchors and workflow evidence gaps"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunCheckpointBuilder {
    checkpoint: RunCheckpoint,
}

impl RunCheckpointBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn objective(mut self, objective: Option<String>) -> Self {
        self.checkpoint.objective = objective.map(|value| compact_anchor(&value));
        self
    }

    pub(crate) fn task_mode(mut self, mode: TaskMode) -> Self {
        self.checkpoint.task_mode = Some(format!("{mode:?}"));
        self
    }

    pub(crate) fn phase(mut self, phase: impl Into<String>) -> Self {
        self.checkpoint.phase = Some(phase.into());
        self
    }

    pub(crate) fn queue(mut self, queue: Option<TaskQueueStatus>) -> Self {
        if let Some(queue) = queue {
            self.checkpoint.workflow_kinds = workflow_kinds(queue);
            self.checkpoint.queue_status = Some(format!(
                "{}/{} done, {} pending, {} active, {} blocked",
                queue.done, queue.total, queue.pending, queue.in_progress, queue.blocked
            ));
        }
        self
    }

    pub(crate) fn active_files(mut self, files: Vec<String>) -> Self {
        self.checkpoint.active_files = compact_list(files);
        self
    }

    pub(crate) fn changed_files(mut self, files: Vec<String>) -> Self {
        self.checkpoint.changed_files = compact_list(files);
        self
    }

    pub(crate) fn blockers(mut self, blockers: Vec<String>) -> Self {
        self.checkpoint.known_blockers = compact_list(blockers);
        self
    }

    pub(crate) fn failed_verification_anchors(mut self, anchors: Vec<String>) -> Self {
        self.checkpoint.failed_verification_anchors = compact_list(anchors);
        self
    }

    pub(crate) fn latest_relevant_verification_status(mut self, status: Option<String>) -> Self {
        self.checkpoint.latest_relevant_verification_status =
            status.map(|value| compact_anchor(&value));
        self
    }

    pub(crate) fn unresolved_workflow_evidence_gaps(mut self, gaps: Vec<String>) -> Self {
        self.checkpoint.unresolved_workflow_evidence_gaps = compact_list(gaps);
        self
    }

    pub(crate) fn next_required_evidence_action(mut self, next: Option<String>) -> Self {
        self.checkpoint.next_required_evidence_action = next.map(|value| compact_anchor(&value));
        self
    }

    pub(crate) fn low_trust_influence_notes(mut self, notes: Vec<String>) -> Self {
        self.checkpoint.low_trust_influence_notes = compact_list(notes);
        self
    }

    pub(crate) fn receipts(mut self, receipts: &[crate::task_result::ToolActionReceipt]) -> Self {
        let index = ReceiptIndex::new(receipts);
        self.checkpoint.files_inspected = index.local_inspection_anchors();
        self.checkpoint.receipt_issue_anchors = index.failed_or_blocked_anchors();
        self
    }

    pub(crate) fn build(self) -> RunCheckpoint {
        self.checkpoint
    }
}

fn workflow_kinds(queue: TaskQueueStatus) -> Vec<String> {
    let mut kinds = Vec::new();
    if queue.workflow.audit > 0 {
        kinds.push("audit".to_string());
    }
    if queue.workflow.patch > 0 {
        kinds.push("patch".to_string());
    }
    if queue.workflow.test > 0 {
        kinds.push("test".to_string());
    }
    if queue.workflow.commit_review > 0 {
        kinds.push("commit_review".to_string());
    }
    if queue.workflow.release_gate > 0 {
        kinds.push("release_gate".to_string());
    }
    if kinds.is_empty() && queue.total > 0 {
        kinds.push("analysis_only".to_string());
    }
    kinds
}

fn push_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!("{label}: {}", values.join(", ")));
}

fn compact_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .take(MAX_ANCHORS_PER_FIELD)
        .map(|value| compact_anchor(&value))
        .collect()
}

fn compact_anchor(value: &str) -> String {
    truncate_chars(
        value.split_whitespace().collect::<Vec<_>>().join(" "),
        MAX_ANCHOR_CHARS,
    )
}

fn truncate_chars(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_checkpoint_stays_under_hard_budget() {
        let checkpoint = RunCheckpoint {
            objective: Some("x".repeat(10_000)),
            changed_files: vec!["src/lib.rs".repeat(100)],
            unresolved_workflow_evidence_gaps: vec!["gap ".repeat(1_000)],
            ..RunCheckpoint::default()
        };

        let rendered = checkpoint.render_compact();
        assert!(rendered.len() <= MAX_RUN_CHECKPOINT_CHARS);
        checkpoint.validate_budget().unwrap();
    }

    #[test]
    fn run_checkpoint_includes_gaps_changed_files_and_failed_verification() {
        let checkpoint = RunCheckpoint {
            changed_files: vec!["src/lib.rs".to_string()],
            failed_verification_anchors: vec!["cargo test exit 1".to_string()],
            unresolved_workflow_evidence_gaps: vec![
                "missing final relevant passing verification".to_string()
            ],
            ..RunCheckpoint::default()
        };
        let rendered = checkpoint.render_compact();

        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("cargo test"));
        assert!(rendered.contains("missing final relevant"));
    }

    #[test]
    fn run_checkpoint_uses_compact_receipt_anchors_only() {
        let receipts = vec![crate::task_result::ToolActionReceipt::new(
            "bash",
            "verify",
            false,
            crate::task_result::ToolActionOutcome::Failed,
            format!("bash: cargo test {}", "RAW_OUTPUT".repeat(1_000)),
        )];
        let checkpoint = RunCheckpointBuilder::new().receipts(&receipts).build();
        let rendered = checkpoint.render_compact();

        assert!(rendered.contains("receipt issues"));
        assert!(!rendered.contains(&"RAW_OUTPUT".repeat(100)));
    }
}

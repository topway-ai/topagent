use crate::file_util::atomic_write;
use crate::plan::TaskQueueStatus;
use crate::receipt_index::ReceiptIndex;
use crate::run_checkpoint::RunCheckpoint;
use crate::task_result::{
    ExecutionSessionOutcome, TaskResult, ToolActionOutcome, ToolActionReceipt, VerificationCommand,
    WorkflowVerification,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const RUN_EVIDENCE_SCHEMA_VERSION: u32 = 1;
pub const RUN_EVIDENCE_RELATIVE_DIR: &str = ".topagent/run-evidence";
pub const RUN_EVIDENCE_LATEST_RELATIVE_PATH: &str = ".topagent/run-evidence/latest.json";
pub const RUN_EVIDENCE_HISTORY_RELATIVE_DIR: &str = ".topagent/run-evidence/history";
pub const MAX_RECENT_RUN_EVIDENCE_SNAPSHOTS: usize = 10;
pub const MAX_RUN_EVIDENCE_ANCHORS: usize = 12;
pub const MAX_RUN_EVIDENCE_ANCHOR_CHARS: usize = 180;
pub const MAX_RUN_EVIDENCE_FINAL_SUMMARY_CHARS: usize = 600;
pub const MAX_RUN_EVIDENCE_HUMAN_CHARS: usize = 10_000;
pub const MAX_RESUME_PROMPT_CHARS: usize = 6_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunEvidenceStatus {
    #[default]
    NotAttempted,
    Blocked,
    Failed,
    Passed,
    Unverified,
    Recovered,
    Satisfied,
    Incomplete,
}

impl RunEvidenceStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Passed => "passed",
            Self::Unverified => "unverified",
            Self::Recovered => "recovered",
            Self::Satisfied => "satisfied",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceSnapshot {
    pub schema_version: u32,
    pub run_id: String,
    pub created_at_unix_millis: u128,
    pub workspace: RunEvidenceWorkspaceState,
    pub session_outcome: ExecutionSessionOutcome,
    pub status: RunEvidenceStatus,
    pub objective: Option<String>,
    pub task_mode: Option<String>,
    pub phase: Option<String>,
    pub queue: Option<TaskQueueStatus>,
    pub queue_status: Option<String>,
    pub workflow: Option<RunEvidenceWorkflowSummary>,
    pub files: RunEvidenceFileSet,
    pub commands: Vec<RunEvidenceCommand>,
    pub receipts: RunEvidenceReceiptSummary,
    pub issues: Vec<RunEvidenceIssue>,
    pub low_trust_influence_notes: Vec<String>,
    pub final_answer_summary: Option<String>,
    pub checkpoint: RunCheckpoint,
    pub resume_hint: RunEvidenceResumeHint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceSummary {
    pub status: RunEvidenceStatus,
    pub workflow_status: Option<RunEvidenceStatus>,
    pub queue_status: Option<String>,
    pub next_safe_action: Option<String>,
    pub resumable: bool,
}

impl RunEvidenceSnapshot {
    pub fn summary(&self) -> RunEvidenceSummary {
        RunEvidenceSummary {
            status: self.status,
            workflow_status: self.workflow.as_ref().map(|workflow| workflow.status),
            queue_status: self.queue_status.clone(),
            next_safe_action: self.resume_hint.next_safe_action.clone(),
            resumable: self.resume_hint.resumable,
        }
    }

    pub fn from_task_result(
        workspace_root: &Path,
        run_id: impl Into<String>,
        checkpoint: RunCheckpoint,
        queue: Option<TaskQueueStatus>,
        task_result: &TaskResult,
    ) -> Self {
        let run_id = run_id.into();
        let receipts = RunEvidenceReceiptSummary::from_receipts(task_result.tool_receipts());
        let commands = run_evidence_commands(task_result.verification_commands());
        let workflow = task_result
            .workflow_verification()
            .map(RunEvidenceWorkflowSummary::from_workflow);
        let status = status_from_task_result(task_result, workflow.as_ref());
        let files = RunEvidenceFileSet {
            active: checkpoint.active_files.clone(),
            changed: compact_list(task_result.files_changed().to_vec()),
            inspected: inspected_files(task_result.tool_receipts()),
            inspection_anchors: receipts.local_inspection.clone(),
        };
        let issues = issues_from_task_result(task_result, task_result.tool_receipts(), &checkpoint);
        let low_trust_influence_notes = compact_list(checkpoint.low_trust_influence_notes.clone());
        let resume_hint = RunEvidenceResumeHint::from_evidence(status, &checkpoint, &issues);

        Self {
            schema_version: RUN_EVIDENCE_SCHEMA_VERSION,
            run_id,
            created_at_unix_millis: unix_timestamp_millis(),
            workspace: RunEvidenceWorkspaceState::capture(workspace_root),
            session_outcome: task_result.session_outcome(),
            status,
            objective: checkpoint.objective.clone(),
            task_mode: checkpoint.task_mode.clone(),
            phase: checkpoint.phase.clone(),
            queue,
            queue_status: checkpoint.queue_status.clone(),
            workflow,
            files,
            commands,
            receipts,
            issues,
            low_trust_influence_notes,
            final_answer_summary: compact_optional(
                &task_result.outcome_summary,
                MAX_RUN_EVIDENCE_FINAL_SUMMARY_CHARS,
            ),
            checkpoint,
            resume_hint,
        }
    }

    pub fn render_status(&self) -> String {
        let freshness = self.assess_freshness();
        let mut lines = vec![
            format!("Run: {}", self.run_id),
            format!("Status: {}", self.status.label()),
            format!("Session: {}", self.session_outcome.label()),
            format!("Workspace: {}", self.workspace.root),
            format!("Freshness: {}", freshness.short_label()),
        ];
        if let Some(objective) = &self.objective {
            lines.push(format!("Objective: {objective}"));
        }
        if let Some(phase) = &self.phase {
            lines.push(format!("Phase: {phase}"));
        }
        if let Some(queue) = &self.queue_status {
            lines.push(format!("Queue: {queue}"));
        }
        lines.push(format!(
            "Resumable: {}",
            if self.resume_hint.resumable {
                "yes"
            } else {
                "no"
            }
        ));
        if self.resume_hint.requires_operator_confirmation {
            lines.push("Resume confirmation: required".to_string());
        }
        if let Some(next) = &self.resume_hint.next_safe_action {
            lines.push(format!("Next safe action: {next}"));
        }
        truncate_chars(lines.join("\n"), MAX_RUN_EVIDENCE_HUMAN_CHARS)
    }

    pub fn render_proof(&self) -> String {
        let mut out = String::from("Proof of work:\n");
        out.push_str(&format!("- Workflow: {}\n", self.status.label()));
        if let Some(queue) = &self.queue_status {
            out.push_str(&format!("- Queue: {queue}\n"));
        }
        push_joined(&mut out, "Inspected", &self.files.inspected);
        push_joined(&mut out, "Changed", &self.files.changed);
        if self.commands.is_empty() {
            out.push_str("- Verified: not attempted\n");
        } else {
            let commands = self
                .commands
                .iter()
                .map(|command| {
                    format!(
                        "`{}` exit {} ({})",
                        command.command,
                        command.exit_code,
                        command.status.label()
                    )
                })
                .collect::<Vec<_>>();
            push_joined(&mut out, "Verified", &commands);
        }
        push_joined(&mut out, "Failed/blocked", &self.receipts.failed_or_blocked);
        let remaining = self
            .issues
            .iter()
            .filter(|issue| !issue.recovered)
            .map(|issue| issue.summary.clone())
            .collect::<Vec<_>>();
        push_joined(&mut out, "Remaining", &remaining);
        if let Some(next) = &self.resume_hint.next_safe_action {
            out.push_str(&format!("- Next: {next}\n"));
        }
        truncate_chars(out.trim_end().to_string(), MAX_RUN_EVIDENCE_HUMAN_CHARS)
    }

    pub fn render_receipts(&self) -> String {
        let mut out = format!("Receipt summary: {} total\n", self.receipts.total);
        push_groups(&mut out, "By skill", &self.receipts.by_skill);
        push_groups(&mut out, "By phase", &self.receipts.by_phase);
        push_joined(&mut out, "Failed/blocked", &self.receipts.failed_or_blocked);
        push_joined(
            &mut out,
            "Local inspection",
            &self.receipts.local_inspection,
        );
        push_joined(&mut out, "Verification", &self.receipts.verification);
        push_joined(
            &mut out,
            "Low-trust/external",
            &self.receipts.low_trust_or_external,
        );
        if self.receipts.omitted_count > 0 {
            out.push_str(&format!(
                "- Omitted compact anchors: {}\n",
                self.receipts.omitted_count
            ));
        }
        truncate_chars(out.trim_end().to_string(), MAX_RUN_EVIDENCE_HUMAN_CHARS)
    }

    pub fn render_verification(&self) -> String {
        let mut out = String::from("Workflow verification:\n");
        if let Some(workflow) = &self.workflow {
            out.push_str(&format!("- Status: {}\n", workflow.status.label()));
            out.push_str(&format!(
                "- Required evidence present: {}\n",
                workflow.required_verification_present
            ));
            out.push_str(&format!(
                "- Failed verification count: {}\n",
                workflow.failed_verification_count
            ));
            out.push_str(&format!(
                "- Final relevant verification passed: {}\n",
                workflow.final_relevant_verification_passed
            ));
            out.push_str(&format!("- Summary: {}\n", workflow.summary));
        } else {
            out.push_str("- Status: unverified\n");
            out.push_str("- Summary: no workflow verification was attached\n");
        }
        push_joined(
            &mut out,
            "Unresolved gaps",
            &self.checkpoint.unresolved_workflow_evidence_gaps,
        );
        push_joined(
            &mut out,
            "Verification commands",
            &command_labels(&self.commands),
        );
        truncate_chars(out.trim_end().to_string(), MAX_RUN_EVIDENCE_HUMAN_CHARS)
    }

    pub fn render_checkpoint(&self) -> String {
        self.checkpoint.render_compact()
    }

    pub fn render_inspect(&self) -> String {
        let freshness = self.assess_freshness();
        let mut out = String::new();
        out.push_str(&self.render_status());
        out.push_str("\n\n");
        out.push_str(&self.render_proof());
        out.push_str("\n\nCheckpoint:\n");
        out.push_str(&self.render_checkpoint());
        out.push_str("\n\n");
        out.push_str(&self.render_verification());
        out.push_str("\n\n");
        out.push_str(&self.render_receipts());
        out.push_str("\n\nFreshness:\n");
        out.push_str(&freshness.render());
        truncate_chars(out.trim_end().to_string(), MAX_RUN_EVIDENCE_HUMAN_CHARS)
    }

    pub fn assess_freshness(&self) -> RunEvidenceFreshness {
        RunEvidenceFreshness::compare(
            self,
            RunEvidenceWorkspaceState::capture(Path::new(&self.workspace.root)),
        )
    }

    pub fn build_resume_prompt(&self, freshness: &RunEvidenceFreshness) -> String {
        let mut prompt = String::new();
        prompt.push_str("Resume the previous TopAgent run from typed evidence only.\n");
        prompt.push_str(
            "Do not replay or rely on raw transcript history, raw receipts, or full tool output.\n",
        );
        prompt.push_str("All tool execution must still go through Harness and approval requirements still apply.\n\n");
        if let Some(objective) = &self.objective {
            prompt.push_str(&format!("Objective: {objective}\n"));
        }
        prompt.push_str(&format!("Previous run status: {}\n", self.status.label()));
        prompt.push_str(&format!(
            "Previous session outcome: {}\n",
            self.session_outcome.label()
        ));
        if let Some(phase) = &self.phase {
            prompt.push_str(&format!("Previous phase: {phase}\n"));
        }
        if let Some(queue) = &self.queue_status {
            prompt.push_str(&format!("Queue: {queue}\n"));
        }
        if let Some(workflow) = &self.workflow {
            prompt.push_str(&format!("Workflow evidence: {}\n", workflow.summary));
        }
        prompt.push_str("\nTyped checkpoint:\n");
        prompt.push_str(&self.checkpoint.render_compact());
        prompt.push_str("\n\nProof anchors:\n");
        push_joined(&mut prompt, "Inspected", &self.files.inspected);
        push_joined(&mut prompt, "Changed", &self.files.changed);
        push_joined(&mut prompt, "Verification", &command_labels(&self.commands));
        push_joined(
            &mut prompt,
            "Failed/blocked",
            &self.receipts.failed_or_blocked,
        );
        push_joined(
            &mut prompt,
            "Unresolved gaps",
            &self.checkpoint.unresolved_workflow_evidence_gaps,
        );
        prompt.push_str("\nWorkspace freshness:\n");
        prompt.push_str(&freshness.render());
        if freshness.stale_risk {
            prompt.push_str("\nTreat continuity as uncertain and re-inspect before mutating.\n");
        }
        if let Some(next) = &self.resume_hint.next_safe_action {
            prompt.push_str(&format!("\nNext safe action: {next}\n"));
        }
        truncate_chars(prompt.trim_end().to_string(), MAX_RESUME_PROMPT_CHARS)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceWorkspaceState {
    pub root: String,
    pub git_head: Option<String>,
    pub git_branch: Option<String>,
    pub dirty_files: Vec<String>,
}

impl RunEvidenceWorkspaceState {
    pub fn capture(workspace_root: &Path) -> Self {
        Self {
            root: workspace_root.display().to_string(),
            git_head: git_output(workspace_root, &["rev-parse", "HEAD"]),
            git_branch: git_output(workspace_root, &["branch", "--show-current"]),
            dirty_files: git_dirty_files(workspace_root),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunEvidenceFileSet {
    pub active: Vec<String>,
    pub changed: Vec<String>,
    pub inspected: Vec<String>,
    pub inspection_anchors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceWorkflowSummary {
    pub status: RunEvidenceStatus,
    pub summary: String,
    pub required_verification_present: bool,
    pub verification_command_count: usize,
    pub failed_verification_count: usize,
    pub final_verification_passed: bool,
    pub final_relevant_verification_passed: bool,
}

impl RunEvidenceWorkflowSummary {
    fn from_workflow(workflow: &WorkflowVerification) -> Self {
        let status = if workflow.satisfied {
            RunEvidenceStatus::Satisfied
        } else if workflow.queue.blocked > 0 {
            RunEvidenceStatus::Blocked
        } else if workflow.failed_verification_count > 0
            && !workflow.final_relevant_verification_passed
        {
            RunEvidenceStatus::Failed
        } else {
            RunEvidenceStatus::Incomplete
        };
        Self {
            status,
            summary: compact_anchor(&workflow.summary),
            required_verification_present: workflow.required_verification_present,
            verification_command_count: workflow.verification_command_count,
            failed_verification_count: workflow.failed_verification_count,
            final_verification_passed: workflow.final_verification_passed,
            final_relevant_verification_passed: workflow.final_relevant_verification_passed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceCommand {
    pub command: String,
    pub exit_code: i32,
    pub status: RunEvidenceStatus,
    pub relevant: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunEvidenceReceiptSummary {
    pub total: usize,
    pub by_skill: Vec<RunEvidenceReceiptGroup>,
    pub by_phase: Vec<RunEvidenceReceiptGroup>,
    pub failed_or_blocked: Vec<String>,
    pub local_inspection: Vec<String>,
    pub verification: Vec<String>,
    pub low_trust_or_external: Vec<String>,
    pub omitted_count: usize,
}

impl RunEvidenceReceiptSummary {
    fn from_receipts(receipts: &[ToolActionReceipt]) -> Self {
        let index = ReceiptIndex::new(receipts);
        let proof = index.compact_proof_summary();
        Self {
            total: receipts.len(),
            by_skill: group_receipts(index.by_skill()),
            by_phase: group_receipts(index.by_phase()),
            failed_or_blocked: proof.failed_or_blocked,
            local_inspection: proof.local_inspection,
            verification: proof.verification,
            low_trust_or_external: proof.low_trust_or_external,
            omitted_count: proof.omitted_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceReceiptGroup {
    pub label: String,
    pub count: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceIssue {
    pub status: RunEvidenceStatus,
    pub summary: String,
    pub recovered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceResumeHint {
    pub resumable: bool,
    pub requires_operator_confirmation: bool,
    pub reason: Option<String>,
    pub next_safe_action: Option<String>,
}

impl RunEvidenceResumeHint {
    fn from_evidence(
        status: RunEvidenceStatus,
        checkpoint: &RunCheckpoint,
        issues: &[RunEvidenceIssue],
    ) -> Self {
        let risky_issue = issues.iter().find(|issue| {
            !issue.recovered
                && (issue.summary.contains("approval required")
                    || issue.summary.contains("external_send")
                    || issue.summary.contains("upload")
                    || issue.summary.contains("post")
                    || issue.summary.contains("destructive")
                    || issue.summary.contains("write")
                    || issue.summary.contains("edit"))
        });
        let requires_operator_confirmation = risky_issue.is_some();
        let reason = risky_issue.map(|issue| issue.summary.clone());
        let resumable = !matches!(
            status,
            RunEvidenceStatus::Satisfied | RunEvidenceStatus::Passed
        );
        let next_safe_action = checkpoint
            .next_required_evidence_action
            .clone()
            .or_else(|| default_next_action(status, requires_operator_confirmation));
        Self {
            resumable,
            requires_operator_confirmation,
            reason,
            next_safe_action,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunEvidenceFreshnessStatus {
    Same,
    Changed,
    Unknown,
}

impl RunEvidenceFreshnessStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Changed => "changed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEvidenceFreshness {
    pub workspace: RunEvidenceFreshnessStatus,
    pub git_head: RunEvidenceFreshnessStatus,
    pub git_branch: RunEvidenceFreshnessStatus,
    pub dirty_state: RunEvidenceFreshnessStatus,
    pub missing_changed_files: Vec<String>,
    pub snapshot_age_secs: u64,
    pub stale_risk: bool,
    pub notes: Vec<String>,
}

impl RunEvidenceFreshness {
    pub fn compare(snapshot: &RunEvidenceSnapshot, current: RunEvidenceWorkspaceState) -> Self {
        let workspace = if current.root == snapshot.workspace.root {
            RunEvidenceFreshnessStatus::Same
        } else {
            RunEvidenceFreshnessStatus::Changed
        };
        let git_head = compare_optional(&snapshot.workspace.git_head, &current.git_head);
        let git_branch = compare_optional(&snapshot.workspace.git_branch, &current.git_branch);
        let dirty_state = if snapshot.workspace.dirty_files == current.dirty_files {
            RunEvidenceFreshnessStatus::Same
        } else {
            RunEvidenceFreshnessStatus::Changed
        };
        let missing_changed_files = snapshot
            .files
            .changed
            .iter()
            .filter(|path| !Path::new(&snapshot.workspace.root).join(path).exists())
            .cloned()
            .collect::<Vec<_>>();
        let snapshot_age_secs = unix_timestamp_millis()
            .saturating_sub(snapshot.created_at_unix_millis)
            .checked_div(1000)
            .and_then(|age| u64::try_from(age).ok())
            .unwrap_or(u64::MAX);
        let stale_risk = workspace == RunEvidenceFreshnessStatus::Changed
            || git_head == RunEvidenceFreshnessStatus::Changed
            || git_branch == RunEvidenceFreshnessStatus::Changed
            || dirty_state == RunEvidenceFreshnessStatus::Changed
            || !missing_changed_files.is_empty();
        let mut notes = Vec::new();
        if workspace == RunEvidenceFreshnessStatus::Changed {
            notes.push("workspace path changed since snapshot".to_string());
        }
        if git_head == RunEvidenceFreshnessStatus::Changed {
            notes.push("git HEAD changed since snapshot".to_string());
        }
        if git_branch == RunEvidenceFreshnessStatus::Changed {
            notes.push("git branch changed since snapshot".to_string());
        }
        if dirty_state == RunEvidenceFreshnessStatus::Changed {
            notes.push("dirty file set changed since snapshot".to_string());
        }
        if !missing_changed_files.is_empty() {
            notes.push(format!(
                "snapshot changed file(s) missing: {}",
                missing_changed_files.join(", ")
            ));
        }
        Self {
            workspace,
            git_head,
            git_branch,
            dirty_state,
            missing_changed_files,
            snapshot_age_secs,
            stale_risk,
            notes,
        }
    }

    pub fn short_label(&self) -> &'static str {
        if self.stale_risk {
            "stale risk"
        } else {
            "fresh"
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("- Workspace: {}\n", self.workspace.label()));
        out.push_str(&format!("- Git HEAD: {}\n", self.git_head.label()));
        out.push_str(&format!("- Git branch: {}\n", self.git_branch.label()));
        out.push_str(&format!("- Dirty state: {}\n", self.dirty_state.label()));
        out.push_str(&format!("- Snapshot age: {}s\n", self.snapshot_age_secs));
        if self.notes.is_empty() {
            out.push_str("- Stale risk: none detected\n");
        } else {
            out.push_str("- Stale risk:\n");
            for note in &self.notes {
                out.push_str(&format!("  - {note}\n"));
            }
        }
        out.trim_end().to_string()
    }
}

#[derive(Debug, Clone)]
pub struct RunEvidenceStore {
    workspace_root: PathBuf,
}

impl RunEvidenceStore {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    pub fn latest_path(&self) -> PathBuf {
        self.workspace_root.join(RUN_EVIDENCE_LATEST_RELATIVE_PATH)
    }

    pub fn history_dir(&self) -> PathBuf {
        self.workspace_root.join(RUN_EVIDENCE_HISTORY_RELATIVE_DIR)
    }

    pub fn load_latest(&self) -> Result<Option<RunEvidenceSnapshot>> {
        let path = self.latest_path();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read latest run evidence {}: {err}",
                path.display()
            ))
        })?;
        let snapshot = serde_json::from_str(&raw).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to parse latest run evidence {}: {err}",
                path.display()
            ))
        })?;
        Ok(Some(snapshot))
    }

    pub fn require_latest(&self) -> Result<RunEvidenceSnapshot> {
        self.load_latest()?.ok_or_else(|| {
            Error::ToolFailed(format!(
                "no run evidence found at {}; run a task first",
                self.latest_path().display()
            ))
        })
    }

    pub fn write_latest(&self, snapshot: &RunEvidenceSnapshot) -> Result<()> {
        let json = serde_json::to_string_pretty(snapshot).map_err(|err| {
            Error::ToolFailed(format!("failed to serialize run evidence snapshot: {err}"))
        })?;
        atomic_write(&self.latest_path(), &(json.clone() + "\n"))?;
        let history_path = self
            .history_dir()
            .join(format!("{}.json", safe_run_id(&snapshot.run_id)));
        atomic_write(&history_path, &(json + "\n"))?;
        self.prune_history()?;
        Ok(())
    }

    fn prune_history(&self) -> Result<()> {
        let dir = self.history_dir();
        if !dir.is_dir() {
            return Ok(());
        }
        let mut entries = std::fs::read_dir(&dir)
            .map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read run evidence history {}: {err}",
                    dir.display()
                ))
            })?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
            .filter_map(|entry| {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()?;
                Some((modified, entry.path()))
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(modified, _)| *modified);
        let excess = entries
            .len()
            .saturating_sub(MAX_RECENT_RUN_EVIDENCE_SNAPSHOTS);
        for (_, path) in entries.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

fn status_from_task_result(
    task_result: &TaskResult,
    workflow: Option<&RunEvidenceWorkflowSummary>,
) -> RunEvidenceStatus {
    if let Some(workflow) = workflow {
        return workflow.status;
    }
    if task_result.has_operator_visible_receipt_issues() {
        return RunEvidenceStatus::Blocked;
    }
    match task_result.session_outcome() {
        ExecutionSessionOutcome::Completed => {
            if task_result.final_verification_passed() {
                RunEvidenceStatus::Passed
            } else if task_result.verification_commands().is_empty() {
                RunEvidenceStatus::Unverified
            } else {
                RunEvidenceStatus::Failed
            }
        }
        ExecutionSessionOutcome::Stopped | ExecutionSessionOutcome::MaxStepsReached => {
            RunEvidenceStatus::Incomplete
        }
        ExecutionSessionOutcome::Failed => RunEvidenceStatus::Failed,
        ExecutionSessionOutcome::Unknown => RunEvidenceStatus::NotAttempted,
    }
}

fn run_evidence_commands(commands: &[VerificationCommand]) -> Vec<RunEvidenceCommand> {
    commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let status = if command.succeeded {
                RunEvidenceStatus::Passed
            } else if later_successful_same_family(commands, index, &command.command) {
                RunEvidenceStatus::Recovered
            } else {
                RunEvidenceStatus::Failed
            };
            RunEvidenceCommand {
                command: compact_anchor(&command.command),
                exit_code: command.exit_code,
                status,
                relevant: true,
            }
        })
        .take(MAX_RUN_EVIDENCE_ANCHORS)
        .collect()
}

fn later_successful_same_family(
    commands: &[VerificationCommand],
    index: usize,
    command: &str,
) -> bool {
    let family = command_family(command);
    commands
        .iter()
        .skip(index + 1)
        .any(|later| later.succeeded && command_family(&later.command) == family)
}

fn command_family(command: &str) -> String {
    let lower = command.trim().to_ascii_lowercase();
    let words = lower.split_whitespace().take(2).collect::<Vec<_>>();
    words.join(" ")
}

fn command_labels(commands: &[RunEvidenceCommand]) -> Vec<String> {
    commands
        .iter()
        .map(|command| {
            format!(
                "`{}` exit {} ({})",
                command.command,
                command.exit_code,
                command.status.label()
            )
        })
        .collect()
}

fn issues_from_task_result(
    task_result: &TaskResult,
    receipts: &[ToolActionReceipt],
    checkpoint: &RunCheckpoint,
) -> Vec<RunEvidenceIssue> {
    let mut issues = Vec::new();
    for issue in task_result.unresolved_issues() {
        issues.push(RunEvidenceIssue {
            status: issue_status(issue),
            summary: compact_anchor(issue),
            recovered: false,
        });
    }
    for gap in &checkpoint.unresolved_workflow_evidence_gaps {
        let summary = compact_anchor(gap);
        if !issues.iter().any(|issue| issue.summary == summary) {
            issues.push(RunEvidenceIssue {
                status: RunEvidenceStatus::Incomplete,
                summary,
                recovered: false,
            });
        }
    }
    for receipt in receipts
        .iter()
        .filter(|receipt| receipt.outcome != ToolActionOutcome::Succeeded)
    {
        let summary = compact_anchor(&format!(
            "{} {}: {}",
            receipt.tool_name,
            receipt.outcome.label(),
            receipt.summary
        ));
        if !issues.iter().any(|issue| issue.summary == summary) {
            issues.push(RunEvidenceIssue {
                status: match receipt.outcome {
                    ToolActionOutcome::Blocked => RunEvidenceStatus::Blocked,
                    ToolActionOutcome::Failed => RunEvidenceStatus::Failed,
                    ToolActionOutcome::Succeeded => RunEvidenceStatus::Passed,
                },
                summary,
                recovered: false,
            });
        }
    }
    issues.truncate(MAX_RUN_EVIDENCE_ANCHORS);
    issues
}

fn issue_status(issue: &str) -> RunEvidenceStatus {
    let lower = issue.to_ascii_lowercase();
    if lower.contains("blocked") || lower.contains("approval") {
        RunEvidenceStatus::Blocked
    } else if lower.contains("failed") {
        RunEvidenceStatus::Failed
    } else if lower.contains("incomplete") || lower.contains("missing") {
        RunEvidenceStatus::Incomplete
    } else {
        RunEvidenceStatus::Unverified
    }
}

fn inspected_files(receipts: &[ToolActionReceipt]) -> Vec<String> {
    let mut files = receipts
        .iter()
        .filter(|receipt| receipt.outcome == ToolActionOutcome::Succeeded)
        .filter_map(|receipt| match receipt.tool_name.as_str() {
            "read" | "write" | "edit" => receipt
                .summary
                .strip_prefix(&format!("{}: ", receipt.tool_name)),
            _ => None,
        })
        .map(|path| compact_anchor(path.trim()))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.truncate(MAX_RUN_EVIDENCE_ANCHORS);
    files
}

fn group_receipts(
    grouped: BTreeMap<&str, Vec<&ToolActionReceipt>>,
) -> Vec<RunEvidenceReceiptGroup> {
    grouped
        .into_iter()
        .map(|(label, receipts)| {
            let mut group = RunEvidenceReceiptGroup {
                label: label.to_string(),
                count: receipts.len(),
                succeeded: 0,
                failed: 0,
                blocked: 0,
            };
            for receipt in receipts {
                match receipt.outcome {
                    ToolActionOutcome::Succeeded => group.succeeded += 1,
                    ToolActionOutcome::Failed => group.failed += 1,
                    ToolActionOutcome::Blocked => group.blocked += 1,
                }
            }
            group
        })
        .take(MAX_RUN_EVIDENCE_ANCHORS)
        .collect()
}

fn push_groups(out: &mut String, label: &str, groups: &[RunEvidenceReceiptGroup]) {
    if groups.is_empty() {
        return;
    }
    let values = groups
        .iter()
        .map(|group| {
            format!(
                "{}: {} total, {} ok, {} failed, {} blocked",
                group.label, group.count, group.succeeded, group.failed, group.blocked
            )
        })
        .collect::<Vec<_>>();
    push_joined(out, label, &values);
}

fn push_joined(out: &mut String, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    out.push_str(&format!("- {label}: {}\n", values.join("; ")));
}

fn default_next_action(
    status: RunEvidenceStatus,
    requires_operator_confirmation: bool,
) -> Option<String> {
    if requires_operator_confirmation {
        return Some("inspect the blocked/failed action and confirm before resuming".to_string());
    }
    match status {
        RunEvidenceStatus::Incomplete => {
            Some("continue from unresolved workflow evidence gaps".to_string())
        }
        RunEvidenceStatus::Failed => {
            Some("inspect the failure, fix it, and rerun relevant verification".to_string())
        }
        RunEvidenceStatus::Blocked => {
            Some("resolve the blocker or choose a safer alternate action".to_string())
        }
        RunEvidenceStatus::Unverified => {
            Some("run relevant local verification before claiming completion".to_string())
        }
        RunEvidenceStatus::NotAttempted => {
            Some("start by inspecting the workspace and plan state".to_string())
        }
        RunEvidenceStatus::Recovered | RunEvidenceStatus::Passed | RunEvidenceStatus::Satisfied => {
            None
        }
    }
}

fn compare_optional(before: &Option<String>, after: &Option<String>) -> RunEvidenceFreshnessStatus {
    match (before, after) {
        (Some(before), Some(after)) if before == after => RunEvidenceFreshnessStatus::Same,
        (Some(_), Some(_)) => RunEvidenceFreshnessStatus::Changed,
        (None, None) => RunEvidenceFreshnessStatus::Unknown,
        _ => RunEvidenceFreshnessStatus::Changed,
    }
}

fn git_output(workspace_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn git_dirty_files(workspace_root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for args in [
        ["diff", "--name-only", "HEAD"].as_slice(),
        ["ls-files", "--others", "--exclude-standard"].as_slice(),
    ] {
        if let Some(output) = git_output(workspace_root, args) {
            for line in output.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with(".topagent/")
                    && !files.iter().any(|file| file == trimmed)
                {
                    files.push(trimmed.to_string());
                }
            }
        }
    }
    files.sort();
    files
}

fn compact_optional(text: &str, max_chars: usize) -> Option<String> {
    let compact = compact_text(text, max_chars);
    (!compact.is_empty()).then_some(compact)
}

fn compact_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .take(MAX_RUN_EVIDENCE_ANCHORS)
        .map(|value| compact_anchor(&value))
        .collect()
}

fn compact_anchor(text: &str) -> String {
    compact_text(text, MAX_RUN_EVIDENCE_ANCHOR_CHARS)
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let redacted = redact_secret_like(text);
    truncate_chars(
        redacted.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
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

fn redact_secret_like(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            let key_value_secret = [
                "token=",
                "api_key=",
                "apikey=",
                "password=",
                "secret=",
                "authorization:",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            if key_value_secret
                || lower.starts_with("sk-")
                || lower.contains("topagent_secret")
                || lower.contains("supersecret")
            {
                "[REDACTED_SECRET]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn safe_run_id(run_id: &str) -> String {
    let safe = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if safe.trim_matches('-').is_empty() {
        format!("run-{}", unix_timestamp_millis())
    } else {
        safe
    }
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_result::{ToolActionOutcome, ToolActionReceipt, VerificationCommand};

    fn sample_checkpoint() -> RunCheckpoint {
        RunCheckpoint {
            objective: Some("fix tests".to_string()),
            phase: Some("Verify".to_string()),
            queue_status: Some("1/2 done, 1 pending, 0 active, 0 blocked".to_string()),
            active_files: vec!["src/lib.rs".to_string()],
            changed_files: vec!["src/lib.rs".to_string()],
            files_inspected: vec!["src/lib.rs".to_string()],
            failed_verification_anchors: vec!["cargo test exit 1".to_string()],
            unresolved_workflow_evidence_gaps: vec![
                "missing final relevant passing verification".to_string()
            ],
            next_required_evidence_action: Some("rerun cargo test".to_string()),
            ..RunCheckpoint::default()
        }
    }

    #[test]
    fn snapshot_includes_workflow_queue_files_and_failed_verification() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("src_lib.rs"), "test").unwrap();
        let mut queue = TaskQueueStatus {
            total: 2,
            done: 1,
            pending: 1,
            ..TaskQueueStatus::default()
        };
        queue.workflow.patch = 1;
        let workflow = WorkflowVerification {
            queue,
            verification_command_count: 1,
            final_verification_passed: false,
            required_verification_present: true,
            failed_verification_count: 1,
            final_relevant_verification_passed: false,
            satisfied: false,
            summary: "final relevant verification did not pass".to_string(),
        };
        let result = TaskResult::new("not done".to_string())
            .with_files_changed(vec!["src/lib.rs".to_string()])
            .with_verification_command(VerificationCommand {
                command: "cargo test".to_string(),
                output: "RAW TOOL OUTPUT SHOULD NOT PERSIST".to_string(),
                exit_code: 1,
                succeeded: false,
            })
            .with_workflow_verification(workflow);

        let snapshot = RunEvidenceSnapshot::from_task_result(
            temp.path(),
            "run-1",
            sample_checkpoint(),
            Some(queue),
            &result,
        );

        assert_eq!(snapshot.status, RunEvidenceStatus::Failed);
        assert_eq!(snapshot.queue.unwrap().pending, 1);
        assert!(snapshot.files.changed.contains(&"src/lib.rs".to_string()));
        assert_eq!(snapshot.commands[0].status, RunEvidenceStatus::Failed);
        assert!(snapshot
            .checkpoint
            .unresolved_workflow_evidence_gaps
            .contains(&"missing final relevant passing verification".to_string()));
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("RAW TOOL OUTPUT SHOULD NOT PERSIST"));
    }

    #[test]
    fn snapshot_includes_blocked_approval_anchor_and_redacts_secret_like_content() {
        let temp = tempfile::tempdir().unwrap();
        let result =
            TaskResult::new("blocked".to_string()).with_tool_receipt(ToolActionReceipt::new(
                "external_send",
                "patch",
                false,
                ToolActionOutcome::Blocked,
                "approval required: upload token=supersecret to https://example.invalid",
            ));

        let snapshot = RunEvidenceSnapshot::from_task_result(
            temp.path(),
            "run-2",
            sample_checkpoint(),
            None,
            &result,
        );

        assert_eq!(snapshot.status, RunEvidenceStatus::Blocked);
        assert!(snapshot.resume_hint.requires_operator_confirmation);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("[REDACTED_SECRET]"));
        assert!(!json.contains("supersecret"));
        assert!(!json.contains("raw_receipts"));
        assert!(!json.contains("transcript"));
    }

    #[test]
    fn snapshot_json_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let result = TaskResult::new("done".to_string()).with_tool_receipt(ToolActionReceipt::new(
            "read",
            "investigate",
            true,
            ToolActionOutcome::Succeeded,
            "read: README.md",
        ));
        let snapshot = RunEvidenceSnapshot::from_task_result(
            temp.path(),
            "run-3",
            sample_checkpoint(),
            None,
            &result,
        );

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let decoded: RunEvidenceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.run_id, "run-3");
        assert!(decoded.files.inspected.contains(&"README.md".to_string()));
    }

    #[test]
    fn store_writes_latest_and_bounds_history() {
        let temp = tempfile::tempdir().unwrap();
        let store = RunEvidenceStore::new(temp.path());
        let result = TaskResult::new("done".to_string());
        for idx in 0..(MAX_RECENT_RUN_EVIDENCE_SNAPSHOTS + 2) {
            let snapshot = RunEvidenceSnapshot::from_task_result(
                temp.path(),
                format!("run-{idx}"),
                sample_checkpoint(),
                None,
                &result,
            );
            store.write_latest(&snapshot).unwrap();
        }

        assert!(store.latest_path().is_file());
        assert_eq!(store.load_latest().unwrap().unwrap().run_id, "run-11");
        let history_count = std::fs::read_dir(store.history_dir()).unwrap().count();
        assert!(history_count <= MAX_RECENT_RUN_EVIDENCE_SNAPSHOTS);
    }

    #[test]
    fn missing_and_corrupted_snapshot_return_useful_results() {
        let temp = tempfile::tempdir().unwrap();
        let store = RunEvidenceStore::new(temp.path());
        assert!(store.load_latest().unwrap().is_none());
        assert!(store
            .require_latest()
            .unwrap_err()
            .to_string()
            .contains("no run evidence found"));

        std::fs::create_dir_all(temp.path().join(RUN_EVIDENCE_RELATIVE_DIR)).unwrap();
        std::fs::write(store.latest_path(), "{not json").unwrap();
        let err = store.load_latest().unwrap_err().to_string();
        assert!(err.contains("failed to parse latest run evidence"));
    }

    #[test]
    fn resume_prompt_uses_typed_evidence_and_stays_under_budget() {
        let temp = tempfile::tempdir().unwrap();
        let result =
            TaskResult::new("done".repeat(1_000)).with_tool_receipt(ToolActionReceipt::new(
                "read",
                "investigate",
                true,
                ToolActionOutcome::Succeeded,
                "read: README.md",
            ));
        let snapshot = RunEvidenceSnapshot::from_task_result(
            temp.path(),
            "run-4",
            sample_checkpoint(),
            None,
            &result,
        );

        let prompt = snapshot.build_resume_prompt(&snapshot.assess_freshness());
        assert!(prompt.len() <= MAX_RESUME_PROMPT_CHARS);
        assert!(prompt.contains("typed evidence only"));
        assert!(!prompt.contains("telegram-history"));
        assert!(!prompt.contains("RAW TOOL OUTPUT"));
    }
}

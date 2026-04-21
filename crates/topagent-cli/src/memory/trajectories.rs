use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use topagent_core::{RunTrustContext, SourceLabel, TaskMode, ToolTraceStep, VerificationCommand};

use super::{slugify, unix_timestamp_secs};

use crate::managed_files::write_managed_file;

const TRAJECTORY_VERSION: u32 = 1;
pub(crate) const TRAJECTORY_EXPORTS_RELATIVE_DIR: &str = ".topagent/exports/trajectories";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrajectoryArtifact {
    pub(crate) version: u32,
    pub(crate) id: String,
    pub(crate) saved_at_unix_secs: i64,
    pub(crate) task_intent: String,
    pub(crate) task_mode: String,
    pub(crate) plan_summary: Vec<String>,
    pub(crate) tool_sequence: Vec<TrajectoryToolStep>,
    pub(crate) changed_files: Vec<String>,
    pub(crate) verification: Vec<TrajectoryVerification>,
    pub(crate) outcome_summary: String,
    #[serde(alias = "lesson_file")]
    pub(crate) note_file: Option<String>,
    pub(crate) procedure_file: Option<String>,
    pub(crate) redaction: TrajectoryRedaction,
    #[serde(default)]
    pub(crate) source_labels: Vec<SourceLabel>,
    #[serde(default)]
    pub(crate) governance: TrajectoryGovernance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrajectoryToolStep {
    pub(crate) tool_name: String,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrajectoryVerification {
    pub(crate) command: String,
    pub(crate) exit_code: i32,
    pub(crate) succeeded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TrajectoryRedaction {
    pub(crate) secret_safe: bool,
    pub(crate) stored_outputs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrajectoryReviewState {
    #[default]
    LocalOnly,
    ReadyForExport,
    Exported,
}

impl TrajectoryReviewState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::ReadyForExport => "ready_for_export",
            Self::Exported => "exported",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct TrajectoryGovernance {
    #[serde(default)]
    pub(crate) review_state: TrajectoryReviewState,
    pub(crate) reviewed_at_unix_secs: Option<i64>,
    pub(crate) exported_at_unix_secs: Option<i64>,
    pub(crate) exported_file: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TrajectoryDraft {
    pub(crate) task_intent: String,
    pub(crate) task_mode: TaskMode,
    pub(crate) plan_summary: Vec<String>,
    pub(crate) tool_sequence: Vec<ToolTraceStep>,
    pub(crate) changed_files: Vec<String>,
    pub(crate) verification: Vec<VerificationCommand>,
    pub(crate) outcome_summary: String,
    pub(crate) note_file: Option<String>,
    pub(crate) procedure_file: Option<String>,
    pub(crate) source_labels: Vec<SourceLabel>,
}

pub(crate) fn save_trajectory(
    trajectories_dir: &Path,
    draft: &TrajectoryDraft,
) -> Result<(String, PathBuf)> {
    std::fs::create_dir_all(trajectories_dir)
        .with_context(|| format!("failed to create {}", trajectories_dir.display()))?;

    let timestamp = unix_timestamp_secs();
    let id = format!("trj-{}-{}", timestamp, slugify(&draft.task_intent, "trajectory"));
    let filename = format!("{id}.json");
    let path = trajectories_dir.join(&filename);

    let artifact = TrajectoryArtifact {
        version: TRAJECTORY_VERSION,
        id,
        saved_at_unix_secs: timestamp,
        task_intent: draft.task_intent.clone(),
        task_mode: match draft.task_mode {
            TaskMode::PlanAndExecute => "execute".to_string(),
            TaskMode::InspectOnly => "inspect".to_string(),
            TaskMode::VerifyOnly => "verify".to_string(),
        },
        plan_summary: draft.plan_summary.clone(),
        tool_sequence: draft
            .tool_sequence
            .iter()
            .take(12)
            .map(|step| TrajectoryToolStep {
                tool_name: step.tool_name.clone(),
                summary: step.summary.clone(),
            })
            .collect(),
        changed_files: draft.changed_files.clone(),
        verification: draft
            .verification
            .iter()
            .map(|command| TrajectoryVerification {
                command: command.command.clone(),
                exit_code: command.exit_code,
                succeeded: command.succeeded,
            })
            .collect(),
        outcome_summary: draft.outcome_summary.clone(),
        note_file: draft.note_file.clone(),
        procedure_file: draft.procedure_file.clone(),
        redaction: TrajectoryRedaction {
            secret_safe: true,
            stored_outputs: false,
        },
        source_labels: draft.source_labels.clone(),
        governance: TrajectoryGovernance::default(),
    };

    write_trajectory_artifact(&path, &artifact)?;
    Ok((format!(".topagent/trajectories/{filename}"), path))
}

pub(crate) fn parse_saved_trajectory(path: &Path) -> Result<Option<TrajectoryArtifact>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let artifact = serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode {}", path.display()))?;
    Ok(Some(artifact))
}

pub(crate) fn mark_trajectory_ready(path: &Path) -> Result<Option<String>> {
    let Some(mut artifact) = parse_saved_trajectory(path)? else {
        return Ok(None);
    };
    ensure_export_quality(&artifact)?;
    artifact.governance.review_state = TrajectoryReviewState::ReadyForExport;
    artifact.governance.reviewed_at_unix_secs = Some(unix_timestamp_secs());
    write_trajectory_artifact(path, &artifact)?;
    Ok(Some(format!(
        ".topagent/trajectories/{}",
        path.file_name().unwrap().to_string_lossy()
    )))
}

pub(crate) fn export_trajectory(workspace_root: &Path, path: &Path) -> Result<Option<String>> {
    let Some(mut artifact) = parse_saved_trajectory(path)? else {
        return Ok(None);
    };
    ensure_export_quality(&artifact)?;
    if artifact.governance.review_state != TrajectoryReviewState::ReadyForExport {
        anyhow::bail!("trajectory must be reviewed and marked ready before export");
    }

    let exports_dir = workspace_root.join(TRAJECTORY_EXPORTS_RELATIVE_DIR);
    std::fs::create_dir_all(&exports_dir)
        .with_context(|| format!("failed to create {}", exports_dir.display()))?;
    let export_path = exports_dir.join(path.file_name().unwrap());
    let exported_at = unix_timestamp_secs();
    artifact.governance.review_state = TrajectoryReviewState::Exported;
    artifact.governance.exported_at_unix_secs = Some(exported_at);
    artifact.governance.exported_file = Some(format!(
        "{}/{}",
        TRAJECTORY_EXPORTS_RELATIVE_DIR,
        export_path.file_name().unwrap().to_string_lossy()
    ));
    write_trajectory_artifact(&export_path, &artifact)?;
    write_trajectory_artifact(path, &artifact)?;
    Ok(artifact.governance.exported_file)
}

fn ensure_export_quality(artifact: &TrajectoryArtifact) -> Result<()> {
    if !artifact.redaction.secret_safe || artifact.redaction.stored_outputs {
        anyhow::bail!("trajectory is not secret-safe for export");
    }
    let trust_context = RunTrustContext {
        sources: artifact.source_labels.clone(),
    };
    if trust_context.has_low_trust_action_influence() {
        let summary = trust_context
            .low_trust_action_summary(2)
            .unwrap_or_else(|| "low-trust content".to_string());
        anyhow::bail!(
            "trajectory is still influenced by low-trust content from: {}",
            summary
        );
    }
    if artifact.verification.is_empty() {
        anyhow::bail!("trajectory has no verification evidence");
    }
    if artifact.tool_sequence.len() < 3 {
        anyhow::bail!("trajectory is too weak to export");
    }
    Ok(())
}

fn write_trajectory_artifact(path: &Path, artifact: &TrajectoryArtifact) -> Result<()> {
    let json = serde_json::to_string_pretty(artifact)
        .with_context(|| format!("failed to encode {}", path.display()))?;
    write_managed_file(path, &json, false)
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use topagent_core::{InfluenceMode, SourceKind, TaskMode, ToolTraceStep, VerificationCommand};

    fn sample_draft() -> TrajectoryDraft {
        TrajectoryDraft {
            task_intent: "Repair the approval mailbox workflow".to_string(),
            task_mode: TaskMode::PlanAndExecute,
            plan_summary: vec![
                "Inspect the workflow".to_string(),
                "Patch it".to_string(),
                "Rerun verification".to_string(),
            ],
            tool_sequence: vec![
                ToolTraceStep {
                    tool_name: "read".to_string(),
                    summary: "read approval.rs".to_string(),
                },
                ToolTraceStep {
                    tool_name: "edit".to_string(),
                    summary: "edit approval.rs".to_string(),
                },
                ToolTraceStep {
                    tool_name: "bash".to_string(),
                    summary: "verification: cargo test -p topagent-core".to_string(),
                },
            ],
            changed_files: vec!["crates/topagent-core/src/approval.rs".to_string()],
            verification: vec![VerificationCommand {
                command: "cargo test -p topagent-core".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            }],
            outcome_summary: "Repaired the workflow and reran verification.".to_string(),
            note_file: None,
            procedure_file: None,
            source_labels: Vec::new(),
        }
    }

    #[test]
    fn test_mark_trajectory_ready_refuses_low_trust_artifact() {
        let temp = TempDir::new().unwrap();
        let trajectories_dir = temp.path().join(".topagent/trajectories");
        let mut draft = sample_draft();
        draft.source_labels.push(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "2 prior transcript snippet(s)",
        ));
        let (_saved, path) = save_trajectory(&trajectories_dir, &draft).unwrap();

        let err = mark_trajectory_ready(&path).unwrap_err().to_string();
        assert!(err.contains("low-trust content"));
        assert!(err.contains("prior transcript"));
    }
}

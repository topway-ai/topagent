use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use topagent_core::{TaskMode, ToolTraceStep, VerificationCommand};

use crate::managed_files::write_managed_file;

const TRAJECTORY_VERSION: u32 = 1;

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
    pub(crate) lesson_file: Option<String>,
    pub(crate) procedure_file: Option<String>,
    pub(crate) redaction: TrajectoryRedaction,
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

#[derive(Debug, Clone)]
pub(crate) struct TrajectoryDraft {
    pub(crate) task_intent: String,
    pub(crate) task_mode: TaskMode,
    pub(crate) plan_summary: Vec<String>,
    pub(crate) tool_sequence: Vec<ToolTraceStep>,
    pub(crate) changed_files: Vec<String>,
    pub(crate) verification: Vec<VerificationCommand>,
    pub(crate) outcome_summary: String,
    pub(crate) lesson_file: Option<String>,
    pub(crate) procedure_file: Option<String>,
}

pub(crate) fn save_trajectory(
    trajectories_dir: &Path,
    draft: &TrajectoryDraft,
) -> Result<(String, PathBuf)> {
    std::fs::create_dir_all(trajectories_dir)
        .with_context(|| format!("failed to create {}", trajectories_dir.display()))?;

    let timestamp = unix_timestamp_secs();
    let id = format!("trj-{}-{}", timestamp, slugify(&draft.task_intent));
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
        lesson_file: draft.lesson_file.clone(),
        procedure_file: draft.procedure_file.clone(),
        redaction: TrajectoryRedaction {
            secret_safe: true,
            stored_outputs: false,
        },
    };

    let json = serde_json::to_string_pretty(&artifact)
        .with_context(|| format!("failed to encode {}", path.display()))?;
    write_managed_file(&path, &json, false)?;
    Ok((format!(".topagent/trajectories/{filename}"), path))
}

fn slugify(input: &str) -> String {
    let slug = input
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == ' ' || *ch == '-')
        .collect::<String>()
        .chars()
        .take(48)
        .collect::<String>()
        .replace(' ', "-");
    if slug.is_empty() {
        "trajectory".to_string()
    } else {
        slug
    }
}

fn unix_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

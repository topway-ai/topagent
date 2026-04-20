use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};

use super::artifact_util;
use super::types::TrajectoryCommands;
use crate::config::workspace::resolve_workspace_path;
#[cfg(test)]
use crate::memory::TrajectoryReviewState;
use crate::memory::{
    MEMORY_TRAJECTORIES_RELATIVE_DIR, mark_trajectory_ready, parse_saved_trajectory,
    write_exported_trajectory,
};

pub(crate) fn run_trajectory_command(
    command: TrajectoryCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    match command {
        TrajectoryCommands::List => print!("{}", render_trajectory_list(&workspace)?),
        TrajectoryCommands::Show { id } => {
            print!("{}", render_trajectory_show(&workspace, &id)?)
        }
        TrajectoryCommands::Review { id } => {
            print!("{}", review_trajectory(&workspace, &id)?)
        }
        TrajectoryCommands::Export { id } => {
            print!("{}", export_selected_trajectory(&workspace, &id)?)
        }
    }
    Ok(())
}

fn render_trajectory_list(workspace: &Path) -> Result<String> {
    let mut output = String::new();
    output.push_str("TopAgent trajectory list\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));

    let mut paths =
        artifact_util::list_files(&workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR), "json")?;
    paths.sort_by(|left, right| right.cmp(left));
    if paths.is_empty() {
        output.push_str("No saved trajectories found.\n");
        return Ok(output);
    }

    for path in paths {
        let Some(trajectory) = parse_saved_trajectory(&path)? else {
            continue;
        };
        output.push_str(&format!(
            "- {} | {} | {}\n",
            path.file_name().unwrap().to_string_lossy(),
            trajectory.governance.review_state.as_str(),
            trajectory.task_intent
        ));
    }

    Ok(output)
}

fn render_trajectory_show(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_trajectory_path(workspace, id)?;
    let body =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(format!(
        "TopAgent trajectory show\nWorkspace: {}\nPath: {}\n\n{}",
        workspace.display(),
        path.display(),
        body
    ))
}

fn review_trajectory(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_trajectory_path(workspace, id)?;
    let saved = mark_trajectory_ready(&path)?
        .ok_or_else(|| anyhow!("trajectory `{}` could not be marked ready", id))?;
    Ok(format!(
        "TopAgent trajectory review\nWorkspace: {}\nReady for export: {}\n",
        workspace.display(),
        saved
    ))
}

fn export_selected_trajectory(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_trajectory_path(workspace, id)?;
    let exported = write_exported_trajectory(workspace, &path)?
        .ok_or_else(|| anyhow!("trajectory `{}` could not be exported", id))?;
    Ok(format!(
        "TopAgent trajectory export\nWorkspace: {}\nExported: {}\n",
        workspace.display(),
        exported
    ))
}

fn resolve_trajectory_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    artifact_util::resolve_unique_artifact_path(
        &workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR),
        id,
        "json",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::trajectories::{TrajectoryDraft, save_trajectory};
    use tempfile::TempDir;
    use topagent_core::{Plan, TaskMode, ToolTraceStep, VerificationCommand};

    fn sample_trajectory() -> TrajectoryDraft {
        let mut plan = Plan::new();
        plan.add_item("Inspect the current workflow".to_string());
        plan.add_item("Patch the workflow and rerun checks".to_string());
        TrajectoryDraft {
            task_intent: "Repair the approval mailbox workflow".to_string(),
            task_mode: TaskMode::PlanAndExecute,
            plan_summary: plan
                .items()
                .iter()
                .map(|item| item.description.clone())
                .collect(),
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
                    summary: "verification: cargo test -p topagent-cli".to_string(),
                },
            ],
            changed_files: vec!["crates/topagent-core/src/approval.rs".to_string()],
            verification: vec![VerificationCommand {
                command: "cargo test -p topagent-cli".to_string(),
                exit_code: 0,
                succeeded: true,
                output: "ok".to_string(),
            }],
            outcome_summary: "Hardened the workflow and reran verification.".to_string(),
            lesson_file: None,
            procedure_file: None,
            source_labels: Vec::new(),
        }
    }

    #[test]
    fn test_review_then_export_keeps_local_and_exported_states_distinct() {
        let temp = TempDir::new().unwrap();
        let trajectories_dir = temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR);
        let (saved, path) = save_trajectory(&trajectories_dir, &sample_trajectory()).unwrap();

        let reviewed = review_trajectory(temp.path(), saved.as_str()).unwrap();
        assert!(reviewed.contains("Ready for export"));
        let reviewed_artifact = parse_saved_trajectory(&path).unwrap().unwrap();
        assert_eq!(
            reviewed_artifact.governance.review_state,
            TrajectoryReviewState::ReadyForExport
        );

        let exported = export_selected_trajectory(temp.path(), saved.as_str()).unwrap();
        assert!(exported.contains("Exported: .topagent/exports/trajectories/"));
        let local_artifact = parse_saved_trajectory(&path).unwrap().unwrap();
        assert_eq!(
            local_artifact.governance.review_state,
            TrajectoryReviewState::Exported
        );
        let export_copy = temp
            .path()
            .join(local_artifact.governance.exported_file.unwrap());
        assert!(export_copy.is_file());
    }
}

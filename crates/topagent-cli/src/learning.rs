use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use topagent_core::{
    load_operator_profile, migrate_legacy_operator_preferences, user_profile_path,
};

use crate::config::resolve_workspace_path;
use crate::memory::{
    disable_procedure, mark_trajectory_ready, parse_saved_procedure, parse_saved_trajectory,
    write_exported_trajectory, ProcedureStatus, TrajectoryReviewState, WorkspaceMemory,
    MEMORY_INDEX_RELATIVE_PATH, MEMORY_LESSONS_RELATIVE_DIR, MEMORY_PLANS_RELATIVE_DIR,
    MEMORY_PROCEDURES_RELATIVE_DIR, MEMORY_TOPICS_RELATIVE_DIR, MEMORY_TRAJECTORIES_RELATIVE_DIR,
    TRAJECTORY_EXPORTS_RELATIVE_DIR,
};

pub(crate) fn run_memory_command(
    command: crate::MemoryCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    migrate_profile_if_needed(&workspace)?;
    match command {
        crate::MemoryCommands::Status => print!("{}", render_memory_status(&workspace)?),
    }
    Ok(())
}

pub(crate) fn run_procedure_command(
    command: crate::ProcedureCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    migrate_profile_if_needed(&workspace)?;
    match command {
        crate::ProcedureCommands::List { all } => {
            print!("{}", render_procedure_list(&workspace, all)?)
        }
        crate::ProcedureCommands::Show { id } => {
            print!("{}", render_procedure_show(&workspace, &id)?)
        }
        crate::ProcedureCommands::Prune => print!("{}", prune_procedures(&workspace)?),
        crate::ProcedureCommands::Disable { id, reason } => {
            print!(
                "{}",
                disable_selected_procedure(&workspace, &id, reason.as_deref())?
            )
        }
    }
    Ok(())
}

pub(crate) fn run_trajectory_command(
    command: crate::TrajectoryCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    match command {
        crate::TrajectoryCommands::List => print!("{}", render_trajectory_list(&workspace)?),
        crate::TrajectoryCommands::Show { id } => {
            print!("{}", render_trajectory_show(&workspace, &id)?)
        }
        crate::TrajectoryCommands::Review { id } => {
            print!("{}", review_trajectory(&workspace, &id)?)
        }
        crate::TrajectoryCommands::Export { id } => {
            print!("{}", export_selected_trajectory(&workspace, &id)?)
        }
    }
    Ok(())
}

fn migrate_profile_if_needed(workspace: &Path) -> Result<()> {
    migrate_legacy_operator_preferences(workspace).map_err(|err| anyhow!(err.to_string()))?;
    Ok(())
}

fn render_memory_status(workspace: &Path) -> Result<String> {
    let memory = WorkspaceMemory::new(workspace.to_path_buf());
    let operator_profile =
        load_operator_profile(workspace).map_err(|err| anyhow!(err.to_string()))?;
    let index_entries = memory.index_entry_count().unwrap_or_default();

    let topics = list_files(&workspace.join(MEMORY_TOPICS_RELATIVE_DIR), "md")?;
    let lessons = list_files(&workspace.join(MEMORY_LESSONS_RELATIVE_DIR), "md")?;
    let plans = list_files(&workspace.join(MEMORY_PLANS_RELATIVE_DIR), "md")?;

    let procedures = list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")?;
    let mut active_procedures = 0usize;
    let mut superseded_procedures = 0usize;
    let mut disabled_procedures = 0usize;
    for path in procedures {
        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        match procedure.status {
            ProcedureStatus::Active => active_procedures += 1,
            ProcedureStatus::Superseded => superseded_procedures += 1,
            ProcedureStatus::Disabled => disabled_procedures += 1,
        }
    }

    let trajectories = list_files(&workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR), "json")?;
    let mut local_trajectories = 0usize;
    let mut ready_trajectories = 0usize;
    let mut exported_trajectories = 0usize;
    for path in trajectories {
        let Some(trajectory) = parse_saved_trajectory(&path)? else {
            continue;
        };
        match trajectory.governance.review_state {
            TrajectoryReviewState::LocalOnly => local_trajectories += 1,
            TrajectoryReviewState::ReadyForExport => ready_trajectories += 1,
            TrajectoryReviewState::Exported => exported_trajectories += 1,
        }
    }
    let export_files = list_files(&workspace.join(TRAJECTORY_EXPORTS_RELATIVE_DIR), "json")?;

    let mut output = String::new();
    output.push_str("TopAgent memory status\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    output.push_str(&format!(
        "Operator model: {} preference(s) ({})\n",
        operator_profile.preferences.len(),
        user_profile_path(workspace).display()
    ));
    output.push_str(&format!(
        "Workspace index: {} entries ({})\n",
        index_entries,
        workspace.join(MEMORY_INDEX_RELATIVE_PATH).display()
    ));
    output.push_str(&format!("Topics: {}\n", topics.len()));
    output.push_str(&format!("Lessons: {}\n", lessons.len()));
    output.push_str(&format!("Plans: {}\n", plans.len()));
    output.push_str(&format!(
        "Procedures: {} active, {} superseded, {} disabled\n",
        active_procedures, superseded_procedures, disabled_procedures
    ));
    output.push_str(&format!(
        "Trajectories: {} local, {} ready, {} exported\n",
        local_trajectories, ready_trajectories, exported_trajectories
    ));
    output.push_str(&format!(
        "Exports: {} ({})\n",
        export_files.len(),
        workspace.join(TRAJECTORY_EXPORTS_RELATIVE_DIR).display()
    ));
    Ok(output)
}

fn render_procedure_list(workspace: &Path, all: bool) -> Result<String> {
    let mut output = String::new();
    output.push_str("TopAgent procedure list\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    output.push_str(if all {
        "Showing: active, superseded, and disabled procedures\n"
    } else {
        "Showing: active procedures only\n"
    });

    let mut procedures = load_all_procedures(workspace)?;
    procedures.sort_by(|left, right| right.filename.cmp(&left.filename));
    let filtered = procedures
        .into_iter()
        .filter(|procedure| all || procedure.status == ProcedureStatus::Active)
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        output.push_str("No matching procedures found.\n");
        return Ok(output);
    }

    for procedure in filtered {
        output.push_str(&format!(
            "- {} | {} | reuse {} | rev {} | {}\n",
            procedure.filename,
            procedure.status.as_str(),
            procedure.reuse_count,
            procedure.revision_count,
            procedure.title
        ));
    }

    Ok(output)
}

fn render_procedure_show(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_procedure_path(workspace, id)?;
    let body =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(format!(
        "TopAgent procedure show\nWorkspace: {}\nPath: {}\n\n{}",
        workspace.display(),
        path.display(),
        body
    ))
}

fn prune_procedures(workspace: &Path) -> Result<String> {
    let mut removed = Vec::new();
    for path in list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")? {
        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        if procedure.status == ProcedureStatus::Active {
            continue;
        }
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        removed.push(procedure.filename);
    }

    let mut output = String::new();
    output.push_str("TopAgent procedure prune\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    if removed.is_empty() {
        output.push_str("Removed: 0\n");
        output.push_str("No superseded or disabled procedures needed pruning.\n");
        return Ok(output);
    }

    let memory = WorkspaceMemory::new(workspace.to_path_buf());
    let report = memory.consolidate_memory_if_needed()?;
    output.push_str(&format!("Removed: {}\n", removed.len()));
    for filename in removed {
        output.push_str(&format!("- removed {}\n", filename));
    }
    output.push_str(&format!(
        "Rewrote memory index with {} live entries.\n",
        report.index_entries_after
    ));
    Ok(output)
}

fn disable_selected_procedure(workspace: &Path, id: &str, reason: Option<&str>) -> Result<String> {
    let path = resolve_procedure_path(workspace, id)?;
    let saved = disable_procedure(&path, reason)?
        .ok_or_else(|| anyhow!("procedure `{}` could not be disabled", id))?;
    let memory = WorkspaceMemory::new(workspace.to_path_buf());
    let _ = memory.consolidate_memory_if_needed()?;
    let mut output = String::new();
    output.push_str("TopAgent procedure disable\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    output.push_str(&format!("Disabled: {}\n", saved));
    if let Some(reason) = reason {
        output.push_str(&format!("Reason: {}\n", reason));
    }
    Ok(output)
}

fn render_trajectory_list(workspace: &Path) -> Result<String> {
    let mut output = String::new();
    output.push_str("TopAgent trajectory list\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));

    let mut paths = list_files(&workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR), "json")?;
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

fn load_all_procedures(workspace: &Path) -> Result<Vec<crate::memory::ParsedProcedure>> {
    let mut procedures = Vec::new();
    for path in list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")? {
        if let Some(procedure) = parse_saved_procedure(&path)? {
            procedures.push(procedure);
        }
    }
    Ok(procedures)
}

fn resolve_procedure_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    resolve_unique_artifact_path(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), id, "md")
}

fn resolve_trajectory_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    resolve_unique_artifact_path(
        &workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR),
        id,
        "json",
    )
}

fn resolve_unique_artifact_path(dir: &Path, id: &str, extension: &str) -> Result<PathBuf> {
    let candidates = list_files(dir, extension)?;
    let needle = Path::new(id)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(id);
    let matches = candidates
        .into_iter()
        .filter(|path| {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            filename == needle
                || stem == needle
                || filename.starts_with(needle)
                || stem.starts_with(needle)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(anyhow!("no artifact matched `{}`", id)),
        [path] => Ok(path.clone()),
        many => Err(anyhow!(
            "artifact id `{}` is ambiguous: {}",
            id,
            many.iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn list_files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(extension))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::procedures::{mark_procedure_superseded, save_procedure, ProcedureDraft};
    use crate::memory::trajectories::{save_trajectory, TrajectoryDraft};
    use tempfile::TempDir;
    use topagent_core::{Plan, TaskMode, ToolTraceStep, VerificationCommand};

    #[test]
    fn test_render_memory_status_reports_user_and_workspace_layers() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/plans")).unwrap();
        fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: runtime\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/topics/architecture.md"),
            "# Architecture",
        )
        .unwrap();
        fs::write(temp.path().join(".topagent/lessons/lesson.md"), "# Lesson").unwrap();
        fs::write(temp.path().join(".topagent/plans/plan.md"), "# Plan").unwrap();

        let rendered = render_memory_status(temp.path()).unwrap();

        assert!(rendered.contains("Operator model: 1 preference(s)"));
        assert!(rendered.contains("Workspace index: 1 entries"));
        assert!(rendered.contains("Topics: 1"));
        assert!(rendered.contains("Lessons: 1"));
        assert!(rendered.contains("Plans: 1"));
    }

    #[test]
    fn test_render_procedure_list_hides_non_active_by_default() {
        let temp = TempDir::new().unwrap();
        let procedures_dir = temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR);
        let (active_file, _active_path) =
            save_procedure(&procedures_dir, &sample_procedure("Active workflow")).unwrap();
        let (_stale_file, stale_path) =
            save_procedure(&procedures_dir, &sample_procedure("Old workflow")).unwrap();
        mark_procedure_superseded(&stale_path, &active_file).unwrap();

        let rendered = render_procedure_list(temp.path(), false).unwrap();
        assert!(rendered.contains("Active workflow"));
        assert!(!rendered.contains("Old workflow"));

        let all = render_procedure_list(temp.path(), true).unwrap();
        assert!(all.contains("Old workflow"));
        assert!(all.contains("superseded"));
    }

    #[test]
    fn test_prune_procedures_removes_non_active_files() {
        let temp = TempDir::new().unwrap();
        let procedures_dir = temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR);
        let (active_file, _active_path) =
            save_procedure(&procedures_dir, &sample_procedure("Live workflow")).unwrap();
        let (_stale_file, stale_path) =
            save_procedure(&procedures_dir, &sample_procedure("Stale workflow")).unwrap();
        mark_procedure_superseded(&stale_path, &active_file).unwrap();

        let rendered = prune_procedures(temp.path()).unwrap();

        assert!(rendered.contains("Removed: 1"));
        assert!(procedures_dir
            .join(active_file.trim_start_matches(".topagent/procedures/"))
            .exists());
        assert!(!stale_path.exists());
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

    fn sample_procedure(title: &str) -> ProcedureDraft {
        ProcedureDraft {
            title: title.to_string(),
            when_to_use: "Use when approval mailbox workflow needs repair.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Inspect the mailbox.".to_string(),
                "Run verification.".to_string(),
            ],
            pitfalls: vec!["Do not drop pending approvals.".to_string()],
            verification: "cargo test -p topagent-cli".to_string(),
            source_task: Some(title.to_string()),
            source_lesson: None,
            source_trajectory: None,
            supersedes: None,
        }
    }

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
}

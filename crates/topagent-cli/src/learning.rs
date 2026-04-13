use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use topagent_core::{
    load_operator_profile, migrate_legacy_operator_preferences, user_profile_path,
};

use crate::config::resolve_workspace_path;
use crate::memory::{
    MEMORY_INDEX_RELATIVE_PATH, MEMORY_LESSONS_RELATIVE_DIR, MEMORY_OBSERVATIONS_RELATIVE_DIR,
    MEMORY_PLANS_RELATIVE_DIR, MEMORY_PROCEDURES_RELATIVE_DIR, MEMORY_TOPICS_RELATIVE_DIR,
    MEMORY_TRAJECTORIES_RELATIVE_DIR, ProcedureStatus, TRAJECTORY_EXPORTS_RELATIVE_DIR,
    TrajectoryReviewState, WorkspaceMemory, disable_procedure, mark_trajectory_ready, observation,
    parse_saved_procedure, parse_saved_trajectory, write_exported_trajectory,
};

fn render_memory_recall(workspace: &Path, instruction: &str) -> Result<String> {
    let memory = WorkspaceMemory::new(workspace.to_path_buf());
    let _ = memory.consolidate_memory_if_needed();
    let memory_prompt = memory.build_prompt(instruction, None)?;

    let mut output = String::new();
    output.push_str("TopAgent memory recall\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    output.push_str(&format!("Instruction: {}\n", instruction));
    output.push('\n');

    if memory_prompt.prompt.is_none() && memory_prompt.operator_prompt.is_none() {
        output.push_str("No memory context would be loaded for this instruction.\n");
        return Ok(output);
    }

    let stats = &memory_prompt.stats;

    if !stats.loaded_operator_items.is_empty() {
        output.push_str(&format!(
            "Operator preferences: {}\n",
            stats.loaded_operator_items.join(", ")
        ));
    }

    if stats.index_prompt_bytes > 0 {
        output.push_str(&format!(
            "Index loaded: {} bytes\n",
            stats.index_prompt_bytes
        ));
    }

    if !stats.loaded_items.is_empty() {
        output.push_str(&format!(
            "Recalled items: {}\n",
            stats.loaded_items.join(", ")
        ));
    }

    if !stats.loaded_procedure_files.is_empty() {
        output.push_str(&format!(
            "Procedure files: {}\n",
            stats.loaded_procedure_files.join(", ")
        ));
    }

    if stats.transcript_snippets > 0 {
        output.push_str(&format!(
            "Transcript snippets: {} ({} bytes)\n",
            stats.transcript_snippets, stats.transcript_prompt_bytes
        ));
    }

    if stats.observation_hints_used > 0 {
        output.push_str(&format!(
            "Observation hints used: {}\n",
            stats.observation_hints_used
        ));
    }

    if !stats.provenance_notes.is_empty() {
        output.push('\n');
        output.push_str("Provenance:\n");
        for note in &stats.provenance_notes {
            output.push_str(&format!("  - {}\n", note));
        }
    }

    let trust = &memory_prompt.trust_context;
    if !trust.sources.is_empty() {
        output.push('\n');
        output.push_str("Trust context:\n");
        for source in &trust.sources {
            output.push_str(&format!(
                "  - {} | {} | {} | {}\n",
                source.kind.label(),
                source.trust.label(),
                source.influence.label(),
                source.summary
            ));
        }
    }

    output.push_str(&format!(
        "\nTotal prompt bytes: {}\n",
        stats.total_prompt_bytes
    ));

    Ok(output)
}

pub(crate) fn run_memory_command(
    command: crate::MemoryCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    migrate_profile_if_needed(&workspace)?;
    match command {
        crate::MemoryCommands::Status => print!("{}", render_memory_status(&workspace)?),
        crate::MemoryCommands::Lint => print!("{}", render_memory_lint(&workspace)?),
        crate::MemoryCommands::Recall { instruction } => {
            print!("{}", render_memory_recall(&workspace, &instruction)?)
        }
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

pub(crate) fn run_observation_command(
    command: crate::ObservationCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    match command {
        crate::ObservationCommands::List { limit } => {
            print!("{}", render_observation_list(&workspace, limit)?)
        }
        crate::ObservationCommands::Show { id } => {
            print!("{}", render_observation_show(&workspace, &id)?)
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
    let observation_files = list_files(&workspace.join(MEMORY_OBSERVATIONS_RELATIVE_DIR), "json")?;

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
    output.push_str(&format!("Observations: {}\n", observation_files.len()));
    Ok(output)
}

fn render_memory_lint(workspace: &Path) -> Result<String> {
    let mut output = String::new();
    output.push_str("TopAgent memory lint\n");
    output.push_str(&format!("Workspace: {}\n", workspace.display()));

    let mut findings = Vec::new();

    let user_path = user_profile_path(workspace);
    if user_path.exists() {
        match std::fs::read_to_string(&user_path) {
            Ok(raw) => {
                if raw.len() > 4096 {
                    findings.push(format!(
                        "ERROR USER.md: size {} bytes exceeds 4096 budget",
                        raw.len()
                    ));
                } else if raw.len() > 2048 {
                    findings.push(format!(
                        "WARNING USER.md: size {} bytes exceeds 2048 budget",
                        raw.len()
                    ));
                }
                for issue in crate::doctor::lint_user_md_content(&raw) {
                    findings.push(format!("WARNING USER.md: {}", issue));
                }
                match load_operator_profile(workspace) {
                    Ok(profile) => {
                        if findings.is_empty() {
                            findings.push(format!(
                                "OK USER.md: {} preference(s), {} bytes",
                                profile.preferences.len(),
                                raw.len()
                            ));
                        }
                    }
                    Err(err) => {
                        findings.push(format!("ERROR USER.md: parse error: {}", err));
                    }
                }
            }
            Err(err) => {
                findings.push(format!("ERROR USER.md: cannot read: {}", err));
            }
        }
    } else {
        findings.push("OK USER.md: not present (optional)".to_string());
    }

    let memory_path = workspace.join(MEMORY_INDEX_RELATIVE_PATH);
    if memory_path.exists() {
        match std::fs::read_to_string(&memory_path) {
            Ok(raw) => {
                if raw.len() > 3000 {
                    findings.push(format!(
                        "ERROR MEMORY.md: size {} bytes exceeds 3000 budget",
                        raw.len()
                    ));
                } else if raw.len() > 1500 {
                    findings.push(format!(
                        "WARNING MEMORY.md: size {} bytes exceeds 1500 budget",
                        raw.len()
                    ));
                }
                let entries: Vec<_> = raw
                    .lines()
                    .filter(|line| line.trim().starts_with("- "))
                    .collect();
                if entries.len() > 24 {
                    findings.push(format!(
                        "WARNING MEMORY.md: {} entries exceeds budget of 24",
                        entries.len()
                    ));
                }
                for issue in crate::doctor::lint_memory_md_content(&raw) {
                    findings.push(format!("WARNING MEMORY.md: {}", issue));
                }
                if findings.iter().all(|f| f.starts_with("OK")) {
                    findings.push(format!(
                        "OK MEMORY.md: {} entries, {} bytes",
                        entries.len(),
                        raw.len()
                    ));
                }
            }
            Err(err) => {
                findings.push(format!("ERROR MEMORY.md: cannot read: {}", err));
            }
        }
    } else {
        findings.push("WARNING MEMORY.md: not present".to_string());
    }

    if findings.is_empty() {
        findings.push("OK: no issues found".to_string());
    }

    for finding in &findings {
        output.push_str(finding);
        output.push('\n');
    }

    let errors = findings.iter().filter(|f| f.starts_with("ERROR")).count();
    let warnings = findings.iter().filter(|f| f.starts_with("WARNING")).count();
    let ok = findings.iter().filter(|f| f.starts_with("OK")).count();
    output.push_str(&format!(
        "Summary: {} OK, {} warning(s), {} error(s)\n",
        ok, warnings, errors
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

fn render_observation_list(workspace: &Path, limit: usize) -> Result<String> {
    let obs_dir = workspace.join(MEMORY_OBSERVATIONS_RELATIVE_DIR);
    let observations = observation::scan_observations(&obs_dir, limit)?;
    if observations.is_empty() {
        return Ok(format!(
            "TopAgent observation index\nWorkspace: {}\n\nNo observations found.\n",
            workspace.display()
        ));
    }
    Ok(format!(
        "TopAgent observation index\nWorkspace: {}\nObservations: {}\n\n{}",
        workspace.display(),
        observations.len(),
        observation::render_observation_list(&observations),
    ))
}

fn render_observation_show(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_observation_path(workspace, id)?;
    let record = observation::load_observation(&path)?
        .ok_or_else(|| anyhow!("observation `{}` not found", id))?;
    Ok(format!(
        "TopAgent observation detail\nWorkspace: {}\n\n{}",
        workspace.display(),
        observation::render_observation_detail(&record),
    ))
}

fn resolve_observation_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    resolve_unique_artifact_path(
        &workspace.join(MEMORY_OBSERVATIONS_RELATIVE_DIR),
        id,
        "json",
    )
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
    use crate::memory::procedures::{ProcedureDraft, mark_procedure_superseded, save_procedure};
    use crate::memory::trajectories::{TrajectoryDraft, save_trajectory};
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
        assert!(
            procedures_dir
                .join(active_file.trim_start_matches(".topagent/procedures/"))
                .exists()
        );
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

    #[test]
    fn test_render_memory_lint_clean_memory_and_user() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/plans")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/observations")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: arch | file: topics/arch.md | status: verified | note: layout\n",
        )
        .unwrap();
        fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep it brief.\n",
        )
        .unwrap();

        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("OK"));
        assert!(!output.contains("WARNING"));
        assert!(!output.contains("ERROR"));
    }

    #[test]
    fn test_render_memory_lint_flags_transient_in_memory() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: deploy | file: topics/deploy.md | status: verified | note: task completed successfully\n",
        )
        .unwrap();

        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("WARNING"));
        assert!(output.contains("transient"));
    }

    #[test]
    fn test_render_memory_lint_flags_forbidden_in_user() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## arch\n**Category:** style\n**Updated:** <t:1>\n**Preference:** The architecture uses microservices.\n",
        )
        .unwrap();

        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("WARNING"));
        assert!(output.contains("forbidden"));
    }

    #[test]
    fn test_render_memory_recall_shows_provenance() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/plans")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/observations")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: runtime layout\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/topics/architecture.md"),
            "# Architecture\nruntime layout details",
        )
        .unwrap();

        let output = render_memory_recall(temp.path(), "inspect runtime architecture").unwrap();
        assert!(output.contains("TopAgent memory recall"));
        assert!(output.contains("runtime architecture"));
        assert!(output.contains("Provenance"));
    }

    #[test]
    fn test_render_memory_recall_empty_workspace() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/plans")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/observations")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        let output = render_memory_recall(temp.path(), "something random xyz").unwrap();
        assert!(output.contains("No memory context"));
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

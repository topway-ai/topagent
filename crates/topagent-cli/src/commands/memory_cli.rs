use anyhow::{Context, Result, anyhow};
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use topagent_core::{
    load_operator_profile, migrate_legacy_operator_preferences, user_profile_path,
};

use super::artifact_util;
use super::surface::PRODUCT_NAME;
use super::types::{MemoryCommands, TrajectoryCommands};
use crate::config::workspace::resolve_workspace_path;
use crate::memory::{
    MEMORY_INDEX_RELATIVE_PATH, MEMORY_MD_SIZE_ERROR, MEMORY_MD_SIZE_WARN,
    MEMORY_NOTES_RELATIVE_DIR, MEMORY_PROCEDURES_RELATIVE_DIR,
    MEMORY_TRAJECTORIES_RELATIVE_DIR, ProcedureStatus, TRAJECTORY_EXPORTS_RELATIVE_DIR,
    TrajectoryReviewState, USER_MD_SIZE_ERROR, USER_MD_SIZE_WARN, WorkspaceMemory,
    parse_saved_procedure, parse_saved_trajectory,
};

pub(crate) fn run_memory_command(
    command: MemoryCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    migrate_profile_if_needed(&workspace)?;
    match command {
        MemoryCommands::Status => print!("{}", render_memory_status(&workspace)?),
        MemoryCommands::Lint => print!("{}", render_memory_lint(&workspace)?),
        MemoryCommands::Recall { instruction } => {
            print!("{}", render_memory_recall(&workspace, &instruction)?)
        }
        MemoryCommands::Trajectory(trajectory_cmd) => {
            run_trajectory_command(trajectory_cmd, &workspace)?;
        }
    }
    Ok(())
}

fn run_trajectory_command(command: TrajectoryCommands, workspace: &Path) -> Result<()> {
    match command {
        TrajectoryCommands::List => print!("{}", render_trajectory_list(workspace)?),
        TrajectoryCommands::Show { id } => {
            print!("{}", render_trajectory_show(workspace, &id)?)
        }
        TrajectoryCommands::Review { id } => {
            print!("{}", review_trajectory(workspace, &id)?)
        }
        TrajectoryCommands::Export { id } => {
            print!("{}", export_selected_trajectory(workspace, &id)?)
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

    let notes = super::artifact_util::list_files(&workspace.join(MEMORY_NOTES_RELATIVE_DIR), "md")?;

    let procedures =
        super::artifact_util::list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")?;
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

    let trajectories = super::artifact_util::list_files(
        &workspace.join(MEMORY_TRAJECTORIES_RELATIVE_DIR),
        "json",
    )?;
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
    let export_files =
        super::artifact_util::list_files(&workspace.join(TRAJECTORY_EXPORTS_RELATIVE_DIR), "json")?;

    let mut output = String::new();
    output.push_str(&format!("{PRODUCT_NAME} memory status\n"));
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
    output.push_str(&format!("Notes: {} note(s)\n", notes.len()));
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

fn render_memory_lint(workspace: &Path) -> Result<String> {
    let mut output = String::new();
    output.push_str(&format!("{PRODUCT_NAME} memory lint\n"));
    output.push_str(&format!("Workspace: {}\n", workspace.display()));

    let mut findings = Vec::new();

    let user_path = user_profile_path(workspace);
    if user_path.exists() {
        match std::fs::read_to_string(&user_path) {
            Ok(raw) => {
                if raw.len() > USER_MD_SIZE_ERROR {
                    findings.push(format!(
                        "ERROR USER.md: size {} bytes exceeds {} budget",
                        raw.len(),
                        USER_MD_SIZE_ERROR
                    ));
                } else if raw.len() > USER_MD_SIZE_WARN {
                    findings.push(format!(
                        "WARNING USER.md: size {} bytes exceeds {} budget",
                        raw.len(),
                        USER_MD_SIZE_WARN
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
                if raw.len() > MEMORY_MD_SIZE_ERROR {
                    findings.push(format!(
                        "ERROR MEMORY.md: size {} bytes exceeds {} budget",
                        raw.len(),
                        MEMORY_MD_SIZE_ERROR
                    ));
                } else if raw.len() > MEMORY_MD_SIZE_WARN {
                    findings.push(format!(
                        "WARNING MEMORY.md: size {} bytes exceeds {} budget",
                        raw.len(),
                        MEMORY_MD_SIZE_WARN
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

fn render_memory_recall(workspace: &Path, instruction: &str) -> Result<String> {
    let memory = WorkspaceMemory::new(workspace.to_path_buf());
    let _ = memory.consolidate_memory_if_needed();
    let memory_prompt = memory.build_prompt(instruction, None)?;

    let mut output = String::new();
    output.push_str(&format!("{PRODUCT_NAME} memory recall\n"));
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

fn render_trajectory_list(workspace: &Path) -> Result<String> {
    let mut output = String::new();
    output.push_str(&format!("{PRODUCT_NAME} memory trajectory list\n"));
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
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(format!(
        "{PRODUCT_NAME} memory trajectory show\nWorkspace: {}\nPath: {}\n\n{}",
        workspace.display(),
        path.display(),
        body
    ))
}

fn review_trajectory(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_trajectory_path(workspace, id)?;
    let saved = crate::memory::mark_trajectory_ready(&path)?
        .ok_or_else(|| anyhow!("trajectory `{}` could not be marked ready", id))?;
    Ok(format!(
        "{PRODUCT_NAME} memory trajectory review\nWorkspace: {}\nReady for export: {}\n",
        workspace.display(),
        saved
    ))
}

fn export_selected_trajectory(workspace: &Path, id: &str) -> Result<String> {
    let path = resolve_trajectory_path(workspace, id)?;
    let exported = crate::memory::write_exported_trajectory(workspace, &path)?
        .ok_or_else(|| anyhow!("trajectory `{}` could not be exported", id))?;
    Ok(format!(
        "{PRODUCT_NAME} memory trajectory export\nWorkspace: {}\nExported: {}\n",
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
    use crate::memory::procedures::{ProcedureDraft, save_procedure};
    use tempfile::TempDir;

    #[test]
    fn test_render_memory_status_reports_user_and_workspace_layers() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/notes")).unwrap();
        fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: notes/architecture.md | status: verified | note: runtime\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/notes/architecture.md"),
            "# Architecture",
        )
        .unwrap();

        let rendered = render_memory_status(temp.path()).unwrap();

        assert!(rendered.contains("Operator model: 1 preference(s)"));
        assert!(rendered.contains("Workspace index: 1 entries"));
        assert!(rendered.contains("Notes:"));
        assert!(rendered.contains("1 note(s)"));
    }

    #[test]
    fn test_render_memory_lint_clean_memory_and_user() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: arch | file: notes/arch.md | status: verified | note: layout\n",
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
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: deploy | file: notes/deploy.md | status: verified | note: task completed successfully\n",
        )
        .unwrap();

        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("WARNING"));
        assert!(output.contains("transient"));
    }

    #[test]
    fn test_render_memory_lint_flags_forbidden_in_user() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
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
        fs::create_dir_all(temp.path().join(".topagent/notes")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: notes/architecture.md | status: verified | note: runtime layout\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/notes/architecture.md"),
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
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        let output = render_memory_recall(temp.path(), "something random xyz").unwrap();
        assert!(output.contains("No memory context"));
    }

    #[test]
    fn test_lint_valid_user_md_no_warnings() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep it brief.\n",
        )
        .unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("OK"));
        assert!(!output.contains("WARNING USER.md"));
        assert!(!output.contains("ERROR USER.md"));
    }

    #[test]
    fn test_lint_oversized_user_md_reports_error_or_warning() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        let mut content = String::from(
            "# Operator Model\n\n## big_pref\n**Category:** style\n**Updated:** <t:1>\n**Preference:** ",
        );
        content.push_str(&"x".repeat(4097));
        content.push('\n');
        fs::write(user_profile_path(temp.path()), &content).unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("USER.md"));
        let has_error_or_warning =
            output.contains("ERROR USER.md") || output.contains("WARNING USER.md");
        assert!(
            has_error_or_warning,
            "expected size warning/error for oversized USER.md"
        );
    }

    #[test]
    fn test_lint_memory_md_flags_verbose_instructions() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: deploy | file: notes/deploy.md | status: verified | note: the agent should always deploy carefully\n",
        )
        .unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("WARNING MEMORY.md"));
        assert!(output.contains("verbose/instructional"));
    }

    #[test]
    fn test_lint_memory_md_procedure_redirect() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: deploy procedure | file: procedures/deploy.md | status: verified | note: step-by-step deployment\n",
        )
        .unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("procedure-like"));
    }

    #[test]
    fn test_lint_clean_memory_md_no_warnings() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: arch | file: notes/arch.md | status: verified | note: service layout\n",
        )
        .unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(!output.contains("WARNING MEMORY.md"));
        assert!(!output.contains("ERROR MEMORY.md"));
        assert!(output.contains("OK MEMORY.md"));
    }

    #[test]
    fn test_lint_output_includes_summary_line() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n",
        )
        .unwrap();
        let output = render_memory_lint(temp.path()).unwrap();
        assert!(output.contains("Summary:"));
        assert!(output.contains("OK"));
    }

    #[test]
    fn test_recall_procedure_shows_trust_context() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();

        let draft = ProcedureDraft {
            title: "Deploy rollback playbook".to_string(),
            when_to_use: "Use for deployment rollback scenarios.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Identify the failing service.".to_string(),
                "Roll back.".to_string(),
            ],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test".to_string(),
            source_task: Some("deploy rollback".to_string()),
            source_lesson: None,
            source_trajectory: None,
            supersedes: None,
        };
        let procedures_path = temp.path().join(".topagent/procedures");
        save_procedure(&procedures_path, &draft).unwrap();
        memory.consolidate_memory_if_needed().unwrap();

        let output = render_memory_recall(temp.path(), "deploy rollback service").unwrap();
        assert!(output.contains("Provenance"));
        assert!(output.contains("procedure"));
        assert!(output.contains("advisory"));
        assert!(output.contains("Trust context"));
    }

    #[test]
    fn test_recall_topic_shows_file_path_in_provenance() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent/notes")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: notes/arch.md | status: verified | note: service layout\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/notes/arch.md"),
            "# Architecture\nservice layout details",
        )
        .unwrap();

        let output = render_memory_recall(temp.path(), "inspect service architecture").unwrap();
        assert!(output.contains("Provenance"));
        assert!(output.contains("notes/arch.md"));
    }

    #[test]
    fn test_recall_total_prompt_bytes_is_bounded() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(MEMORY_NOTES_RELATIVE_DIR)).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
        fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
        fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- topic: architecture | file: notes/arch.md | status: verified | note: service layout\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".topagent/notes/arch.md"),
            "# Architecture\nservice layout details",
        )
        .unwrap();

        let output = render_memory_recall(temp.path(), "inspect service architecture").unwrap();
        assert!(output.contains("Total prompt bytes:"));
    }
}

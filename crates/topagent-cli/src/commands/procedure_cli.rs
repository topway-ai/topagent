use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};

use super::artifact_util;
use super::surface::PRODUCT_NAME;
use super::types::ProcedureCommands;
use crate::config::workspace::resolve_workspace_path;
use crate::memory::{
    MEMORY_PROCEDURES_RELATIVE_DIR, ProcedureStatus, WorkspaceMemory, disable_procedure,
    parse_saved_procedure,
};
use topagent_core::migrate_legacy_operator_preferences;

pub(crate) fn run_procedure_command(
    command: ProcedureCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    migrate_legacy_operator_preferences(&workspace).map_err(|err| anyhow!(err.to_string()))?;
    match command {
        ProcedureCommands::List { all } => {
            print!("{}", render_procedure_list(&workspace, all)?)
        }
        ProcedureCommands::Show { id } => {
            print!("{}", render_procedure_show(&workspace, &id)?)
        }
        ProcedureCommands::Prune => print!("{}", prune_procedures(&workspace)?),
        ProcedureCommands::Disable { id, reason } => {
            print!(
                "{}",
                disable_selected_procedure(&workspace, &id, reason.as_deref())?
            )
        }
    }
    Ok(())
}

fn render_procedure_list(workspace: &Path, all: bool) -> Result<String> {
    let mut output = String::new();
    output.push_str(&format!("{PRODUCT_NAME} procedure list\n"));
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
        "{PRODUCT_NAME} procedure show\nWorkspace: {}\nPath: {}\n\n{}",
        workspace.display(),
        path.display(),
        body
    ))
}

fn prune_procedures(workspace: &Path) -> Result<String> {
    let mut removed = Vec::new();
    for path in artifact_util::list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")? {
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
    output.push_str(&format!("{PRODUCT_NAME} procedure prune\n"));
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
    output.push_str(&format!("{PRODUCT_NAME} procedure disable\n"));
    output.push_str(&format!("Workspace: {}\n", workspace.display()));
    output.push_str(&format!("Disabled: {}\n", saved));
    if let Some(reason) = reason {
        output.push_str(&format!("Reason: {}\n", reason));
    }
    Ok(output)
}

fn load_all_procedures(workspace: &Path) -> Result<Vec<crate::memory::ParsedProcedure>> {
    let mut procedures = Vec::new();
    for path in artifact_util::list_files(&workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR), "md")? {
        if let Some(procedure) = parse_saved_procedure(&path)? {
            procedures.push(procedure);
        }
    }
    Ok(procedures)
}

fn resolve_procedure_path(workspace: &Path, id: &str) -> Result<PathBuf> {
    artifact_util::resolve_unique_artifact_path(
        &workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR),
        id,
        "md",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::procedures::{ProcedureDraft, mark_procedure_superseded, save_procedure};
    use tempfile::TempDir;

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
}

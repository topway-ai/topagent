use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use topagent_core::{WorkspaceCheckpointRestoreReport, WorkspaceCheckpointStore};

use crate::config::resolve_workspace_path;
use crate::telegram::clear_workspace_telegram_history;

pub(crate) fn run_checkpoint_command(
    command: crate::CheckpointCommands,
    workspace: Option<PathBuf>,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    let store = WorkspaceCheckpointStore::new(workspace.clone());

    match command {
        crate::CheckpointCommands::Status => render_checkpoint_status(&workspace, &store),
        crate::CheckpointCommands::Diff => render_checkpoint_diff(&workspace, &store),
        crate::CheckpointCommands::Restore => restore_checkpoint(&workspace, &store),
    }
}

fn render_checkpoint_status(workspace: &Path, store: &WorkspaceCheckpointStore) -> Result<()> {
    println!("TopAgent checkpoint status");
    println!("Workspace: {}", workspace.display());

    let Some(status) = store.latest_status()? else {
        println!("Checkpoint: none");
        println!("No active workspace checkpoint found.");
        return Ok(());
    };

    println!("Checkpoint: {}", status.id);
    println!(
        "Created: {}",
        format_checkpoint_time(status.created_at_unix_millis)
    );
    println!("Capture events: {}", status.captures.len());
    for capture in status.captures {
        if let Some(detail) = capture.detail {
            println!(
                "- {}: {} ({})",
                capture.source.label(),
                capture.reason,
                detail
            );
        } else {
            println!("- {}: {}", capture.source.label(), capture.reason);
        }
    }
    println!("Captured paths: {}", status.captured_paths.len());
    for path in status.captured_paths {
        println!("- {}", path);
    }
    Ok(())
}

fn render_checkpoint_diff(workspace: &Path, store: &WorkspaceCheckpointStore) -> Result<()> {
    println!("TopAgent checkpoint diff");
    println!("Workspace: {}", workspace.display());

    let Some(status) = store.latest_status()? else {
        println!("No active workspace checkpoint found.");
        return Ok(());
    };
    println!("Checkpoint: {}", status.id);

    let diff = store
        .latest_diff_preview()?
        .unwrap_or_else(|| "No active workspace checkpoint found.".to_string());
    println!();
    print!("{}", diff);
    if !diff.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn restore_checkpoint(workspace: &Path, store: &WorkspaceCheckpointStore) -> Result<()> {
    let (report, cleared_transcripts) = restore_checkpoint_and_clear_transcripts(workspace, store)?;

    println!("TopAgent checkpoint restore");
    println!("Workspace: {}", workspace.display());
    println!("Checkpoint restored: {}", report.checkpoint_id);
    println!("Restored files: {}", report.restored_files.len());
    for path in report.restored_files {
        println!("- restored {}", path);
    }
    println!("Removed files: {}", report.removed_files.len());
    for path in report.removed_files {
        println!("- removed {}", path);
    }
    if cleared_transcripts {
        println!("Cleared persisted Telegram transcripts for this workspace.");
    } else {
        println!("No persisted Telegram transcripts needed clearing.");
    }
    Ok(())
}

fn restore_checkpoint_and_clear_transcripts(
    workspace: &Path,
    store: &WorkspaceCheckpointStore,
) -> Result<(WorkspaceCheckpointRestoreReport, bool)> {
    let report = store
        .restore_latest()?
        .ok_or_else(|| anyhow!("No active workspace checkpoint found."))?;
    let cleared_transcripts = clear_workspace_telegram_history(workspace)?;
    Ok((report, cleared_transcripts))
}

fn format_checkpoint_time(created_at_unix_millis: u128) -> String {
    let timestamp = i64::try_from(created_at_unix_millis / 1000).unwrap_or(i64::MAX);
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| created_at_unix_millis.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use topagent_core::checkpoint::{CheckpointCaptureMetadata, CheckpointCaptureSource};

    #[test]
    fn test_restore_checkpoint_clears_workspace_telegram_history() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();
        std::fs::write(workspace.join("notes.txt"), "before").unwrap();
        let store = WorkspaceCheckpointStore::new(workspace.to_path_buf());
        store
            .capture_file(
                "notes.txt",
                CheckpointCaptureMetadata::new(CheckpointCaptureSource::Write, "structured write"),
            )
            .unwrap();
        std::fs::write(workspace.join("notes.txt"), "after").unwrap();

        let history_dir = workspace.join(".topagent").join("telegram-history");
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(history_dir.join("chat-1.json"), "{}").unwrap();

        let (report, cleared_transcripts) =
            restore_checkpoint_and_clear_transcripts(workspace, &store).unwrap();

        assert_eq!(report.restored_files, vec!["notes.txt"]);
        assert!(cleared_transcripts);
        assert!(!history_dir.exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
            "before"
        );
    }

    #[test]
    fn test_restore_checkpoint_preserves_durable_learning_artifacts() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();
        let topagent_dir = workspace.join(".topagent");

        // Create durable learning artifacts before checkpoint
        let lessons_dir = topagent_dir.join("lessons");
        let procedures_dir = topagent_dir.join("procedures");
        let observations_dir = topagent_dir.join("observations");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::create_dir_all(&procedures_dir).unwrap();
        std::fs::create_dir_all(&observations_dir).unwrap();
        std::fs::write(
            lessons_dir.join("lesson-1.md"),
            "# Lesson 1\nImportant fact",
        )
        .unwrap();
        std::fs::write(
            procedures_dir.join("proc-1.md"),
            "# Procedure 1\nStep-by-step",
        )
        .unwrap();
        std::fs::write(
            observations_dir.join("obs-1.json"),
            r#"{"task_hash":"abc"}"#,
        )
        .unwrap();
        std::fs::write(topagent_dir.join("MEMORY.md"), "- lesson-1: important fact").unwrap();
        std::fs::write(
            topagent_dir.join("USER.md"),
            "Operator prefers concise replies",
        )
        .unwrap();

        // Capture only a workspace file, not learning artifacts
        std::fs::write(workspace.join("src.rs"), "fn main() {}").unwrap();
        let store = WorkspaceCheckpointStore::new(workspace.to_path_buf());
        store
            .capture_file(
                "src.rs",
                CheckpointCaptureMetadata::new(CheckpointCaptureSource::Write, "code change"),
            )
            .unwrap();

        // Modify the captured file
        std::fs::write(workspace.join("src.rs"), "fn main() { broken() }").unwrap();

        // Add a transcript (should be cleared)
        let history_dir = topagent_dir.join("telegram-history");
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(history_dir.join("chat-42.json"), "[{\"text\":\"hello\"}]").unwrap();

        // Restore
        let (report, cleared_transcripts) =
            restore_checkpoint_and_clear_transcripts(workspace, &store).unwrap();

        assert_eq!(report.restored_files, vec!["src.rs"]);
        assert!(cleared_transcripts);

        // Captured file is restored
        assert_eq!(
            std::fs::read_to_string(workspace.join("src.rs")).unwrap(),
            "fn main() {}"
        );

        // Durable learning artifacts survive
        assert_eq!(
            std::fs::read_to_string(lessons_dir.join("lesson-1.md")).unwrap(),
            "# Lesson 1\nImportant fact"
        );
        assert_eq!(
            std::fs::read_to_string(procedures_dir.join("proc-1.md")).unwrap(),
            "# Procedure 1\nStep-by-step"
        );
        assert_eq!(
            std::fs::read_to_string(observations_dir.join("obs-1.json")).unwrap(),
            r#"{"task_hash":"abc"}"#
        );
        assert_eq!(
            std::fs::read_to_string(topagent_dir.join("MEMORY.md")).unwrap(),
            "- lesson-1: important fact"
        );
        assert_eq!(
            std::fs::read_to_string(topagent_dir.join("USER.md")).unwrap(),
            "Operator prefers concise replies"
        );

        // Transcripts are cleared
        assert!(!history_dir.exists());
    }

    #[test]
    fn test_restore_no_checkpoint_returns_error() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();
        let store = WorkspaceCheckpointStore::new(workspace.to_path_buf());

        let result = restore_checkpoint_and_clear_transcripts(workspace, &store);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No active workspace checkpoint found"));
    }
}

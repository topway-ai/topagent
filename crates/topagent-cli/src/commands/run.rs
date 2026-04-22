use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use topagent_core::{WorkspaceRunSnapshotRestoreReport, WorkspaceRunSnapshotStore};

use crate::commands::surface::PRODUCT_NAME;
use crate::config::workspace::resolve_workspace_path;
use crate::memory::TELEGRAM_HISTORY_RELATIVE_DIR;
use crate::telegram::clear_workspace_telegram_history;

pub(crate) fn run_session_status(workspace_override: Option<PathBuf>) -> Result<()> {
    let workspace = resolve_workspace_path(workspace_override)?;
    print!("{}", render_session_status(&workspace));
    Ok(())
}

pub(crate) fn run_snapshot_diff(workspace_override: Option<PathBuf>) -> Result<()> {
    let workspace = resolve_workspace_path(workspace_override)?;
    let store = WorkspaceRunSnapshotStore::new(workspace.clone());
    println!("{PRODUCT_NAME} run diff");
    println!("Workspace: {}", workspace.display());

    let Some(status) = store.latest_status()? else {
        println!("No active workspace run snapshot found.");
        return Ok(());
    };
    println!("Run snapshot: {}", status.id);

    let diff = store
        .latest_diff_preview()?
        .unwrap_or_else(|| "No active workspace run snapshot found.".to_string());
    println!();
    print!("{}", diff);
    if !diff.ends_with('\n') {
        println!();
    }
    Ok(())
}

pub(crate) fn run_snapshot_restore(workspace_override: Option<PathBuf>) -> Result<()> {
    let workspace = resolve_workspace_path(workspace_override)?;
    let store = WorkspaceRunSnapshotStore::new(workspace.clone());
    let (report, cleared_transcripts) =
        restore_run_snapshot_and_clear_transcripts(&workspace, &store)?;

    println!("{PRODUCT_NAME} run restore");
    println!("Workspace: {}", workspace.display());
    println!("Run snapshot restored: {}", report.snapshot_id);
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

pub(crate) fn render_session_status(workspace: &Path) -> String {
    let mut out = format!("{PRODUCT_NAME} run status\n\n");
    out.push_str(&format!("Workspace: {}\n", workspace.display()));

    let service_state = crate::service::query_service_active_state();
    out.push_str(&format!("\nService state:        {}\n", service_state));

    let store = WorkspaceRunSnapshotStore::new(workspace.to_path_buf());
    match store.latest_status() {
        Ok(Some(status)) => {
            out.push_str(&format!(
                "\nRun snapshot:           present ({})\n",
                status.id
            ));
            let timestamp = format_session_time(status.created_at_unix_millis);
            out.push_str(&format!("  Created:            {}\n", timestamp));
            out.push_str(&format!(
                "  Captured paths:     {}\n",
                status.captured_paths.len()
            ));
        }
        Ok(None) => {
            out.push_str("\nRun snapshot:           none\n");
        }
        Err(err) => {
            out.push_str(&format!("\nRun snapshot:           error — {}\n", err));
        }
    }

    let history_dir = workspace.join(TELEGRAM_HISTORY_RELATIVE_DIR);
    let transcript_count = if history_dir.is_dir() {
        std::fs::read_dir(&history_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .map(|ext| ext == "json")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };
    out.push_str(&format!(
        "\nTelegram transcripts: {} chat file{}\n",
        transcript_count,
        if transcript_count == 1 { "" } else { "s" }
    ));

    let has_snapshot = matches!(store.latest_status(), Ok(Some(_)));
    let has_transcripts = transcript_count > 0;
    if has_snapshot || has_transcripts {
        out.push_str("\nRecovery:\n");
        if has_snapshot {
            out.push_str("  A run snapshot exists. Preview changes with: topagent run diff\n");
            out.push_str("  Restore workspace and clear transcripts:   topagent run restore\n");
        }
        if has_transcripts {
            out.push_str("  Clear per-chat transcripts via Telegram:   /reset (in each chat)\n");
        }
    }

    out.push_str(
        "\nNote: In-flight session state is not persisted. For run logs:\n  \
         journalctl --user -u topagent-telegram.service -n 50\n",
    );

    out
}

pub(crate) fn format_session_time(unix_millis: u128) -> String {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let timestamp = i64::try_from(unix_millis / 1000).unwrap_or(i64::MAX);
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| unix_millis.to_string())
}

fn restore_run_snapshot_and_clear_transcripts(
    workspace: &Path,
    store: &WorkspaceRunSnapshotStore,
) -> Result<(WorkspaceRunSnapshotRestoreReport, bool)> {
    let report = store
        .restore_latest()?
        .ok_or_else(|| anyhow!("No active workspace run snapshot found."))?;
    let cleared_transcripts = clear_workspace_telegram_history(workspace)?;
    Ok((report, cleared_transcripts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use topagent_core::run_snapshot::{RunSnapshotCaptureMetadata, RunSnapshotCaptureSource};

    #[test]
    fn test_restore_run_snapshot_clears_workspace_telegram_history() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();
        std::fs::write(workspace.join("notes.txt"), "before").unwrap();
        let store = WorkspaceRunSnapshotStore::new(workspace.to_path_buf());
        store
            .capture_file(
                "notes.txt",
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Write,
                    "structured write",
                ),
            )
            .unwrap();
        std::fs::write(workspace.join("notes.txt"), "after").unwrap();

        let history_dir = workspace.join(TELEGRAM_HISTORY_RELATIVE_DIR);
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(history_dir.join("chat-1.json"), "{}").unwrap();

        let (report, cleared_transcripts) = restore_run_snapshot_and_clear_transcripts(
            workspace,
            &WorkspaceRunSnapshotStore::new(workspace.to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.restored_files, vec!["notes.txt"]);
        assert!(cleared_transcripts);
        assert!(!history_dir.exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
            "before"
        );
    }

    #[test]
    fn test_restore_run_snapshot_preserves_durable_learning_artifacts() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();
        let topagent_dir = workspace.join(".topagent");

        let notes_dir = topagent_dir.join("notes");
        let procedures_dir = topagent_dir.join("procedures");
        std::fs::create_dir_all(&notes_dir).unwrap();
        std::fs::create_dir_all(&procedures_dir).unwrap();
        std::fs::write(notes_dir.join("note-1.md"), "# Note 1\nImportant fact").unwrap();
        std::fs::write(
            procedures_dir.join("proc-1.md"),
            "# Procedure 1\nStep-by-step",
        )
        .unwrap();
        std::fs::write(topagent_dir.join("MEMORY.md"), "- note-1: important fact").unwrap();
        std::fs::write(
            topagent_dir.join("USER.md"),
            "Operator prefers concise replies",
        )
        .unwrap();

        std::fs::write(workspace.join("src.rs"), "fn main() {}").unwrap();
        let store = WorkspaceRunSnapshotStore::new(workspace.to_path_buf());
        store
            .capture_file(
                "src.rs",
                RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "code change"),
            )
            .unwrap();

        std::fs::write(workspace.join("src.rs"), "fn main() { broken() }").unwrap();

        let history_dir = workspace.join(TELEGRAM_HISTORY_RELATIVE_DIR);
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::write(history_dir.join("chat-42.json"), "[{\"text\":\"hello\"}]").unwrap();

        let (report, cleared_transcripts) = restore_run_snapshot_and_clear_transcripts(
            workspace,
            &WorkspaceRunSnapshotStore::new(workspace.to_path_buf()),
        )
        .unwrap();

        assert_eq!(report.restored_files, vec!["src.rs"]);
        assert!(cleared_transcripts);

        assert_eq!(
            std::fs::read_to_string(workspace.join("src.rs")).unwrap(),
            "fn main() {}"
        );

        assert_eq!(
            std::fs::read_to_string(notes_dir.join("note-1.md")).unwrap(),
            "# Note 1\nImportant fact"
        );
        assert_eq!(
            std::fs::read_to_string(procedures_dir.join("proc-1.md")).unwrap(),
            "# Procedure 1\nStep-by-step"
        );
        assert_eq!(
            std::fs::read_to_string(topagent_dir.join("MEMORY.md")).unwrap(),
            "- note-1: important fact"
        );
        assert_eq!(
            std::fs::read_to_string(topagent_dir.join("USER.md")).unwrap(),
            "Operator prefers concise replies"
        );

        assert!(!history_dir.exists());
    }

    #[test]
    fn test_restore_no_run_snapshot_returns_error() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path();

        let result = restore_run_snapshot_and_clear_transcripts(
            workspace,
            &WorkspaceRunSnapshotStore::new(workspace.to_path_buf()),
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No active workspace run snapshot found"));
    }
}

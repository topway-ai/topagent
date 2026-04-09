use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use topagent_core::WorkspaceCheckpointStore;

use crate::config::resolve_workspace_path;

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
    println!("Captured files: {}", status.captured_paths.len());
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
    let report = store
        .restore_latest()?
        .ok_or_else(|| anyhow!("No active workspace checkpoint found."))?;
    let cleared_transcripts = clear_workspace_telegram_history(workspace)?;

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

fn format_checkpoint_time(created_at_unix_millis: u128) -> String {
    let timestamp = i64::try_from(created_at_unix_millis / 1000).unwrap_or(i64::MAX);
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| created_at_unix_millis.to_string())
}

fn clear_workspace_telegram_history(workspace: &Path) -> Result<bool> {
    let history_dir = workspace.join(".topagent").join("telegram-history");
    if !history_dir.exists() {
        return Ok(false);
    }

    std::fs::remove_dir_all(&history_dir)
        .with_context(|| format!("failed to remove {}", history_dir.display()))?;
    Ok(true)
}

use anyhow::Result;
use std::path::Path;
use topagent_core::WorkspaceCheckpointStore;

pub(crate) fn run_session_status(workspace_override: Option<std::path::PathBuf>) -> Result<()> {
    let workspace = crate::config::resolve_workspace_path(workspace_override)?;
    print!("{}", render_session_status(&workspace));
    Ok(())
}

pub(crate) fn render_session_status(workspace: &Path) -> String {
    let mut out = String::from("TopAgent run status\n\n");
    out.push_str(&format!("Workspace: {}\n", workspace.display()));

    let service_state = crate::service::query_service_active_state();
    out.push_str(&format!("\nService state:        {}\n", service_state));

    let store = WorkspaceCheckpointStore::new(workspace.to_path_buf());
    match store.latest_status() {
        Ok(Some(status)) => {
            out.push_str(&format!(
                "\nCheckpoint:           present ({})\n",
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
            out.push_str("\nCheckpoint:           none\n");
        }
        Err(err) => {
            out.push_str(&format!("\nCheckpoint:           error — {}\n", err));
        }
    }

    let history_dir = workspace.join(".topagent").join("telegram-history");
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

    let has_checkpoint = matches!(store.latest_status(), Ok(Some(_)));
    let has_transcripts = transcript_count > 0;
    if has_checkpoint || has_transcripts {
        out.push_str("\nRecovery:\n");
        if has_checkpoint {
            out.push_str("  A checkpoint exists. Preview changes with: topagent checkpoint diff\n");
            out.push_str(
                "  Restore workspace and clear transcripts:   topagent checkpoint restore\n",
            );
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

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use topagent_core::{
    RunEvidenceFreshness, RunEvidenceSnapshot, RunEvidenceStore, WorkspaceRunSnapshotRestoreReport,
    WorkspaceRunSnapshotStore, MAX_RESUME_PROMPT_CHARS,
};

use crate::commands::surface::PRODUCT_NAME;
use crate::config::defaults::CliParams;
use crate::config::workspace::resolve_workspace_path;
use crate::memory::TELEGRAM_HISTORY_RELATIVE_DIR;
use crate::telegram::clear_workspace_telegram_history;

pub(crate) fn run_session_status(workspace_override: Option<PathBuf>, json: bool) -> Result<()> {
    let workspace = resolve_workspace_path(workspace_override)?;
    if json {
        let snapshot = RunEvidenceStore::new(workspace).load_latest()?;
        print_json(&snapshot)?;
    } else {
        print!("{}", render_session_status(&workspace));
    }
    Ok(())
}

pub(crate) fn run_evidence_proof(workspace_override: Option<PathBuf>, json: bool) -> Result<()> {
    let snapshot = require_latest_run_evidence(workspace_override)?;
    print_evidence_view(&snapshot, json, |snapshot| snapshot.render_proof())
}

pub(crate) fn run_evidence_checkpoint(
    workspace_override: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let snapshot = require_latest_run_evidence(workspace_override)?;
    if json {
        print_json(&snapshot.checkpoint)?;
    } else {
        println!("{}", snapshot.render_checkpoint());
    }
    Ok(())
}

pub(crate) fn run_evidence_receipts(workspace_override: Option<PathBuf>, json: bool) -> Result<()> {
    let snapshot = require_latest_run_evidence(workspace_override)?;
    if json {
        print_json(&snapshot.receipts)?;
    } else {
        println!("{}", snapshot.render_receipts());
    }
    Ok(())
}

pub(crate) fn run_evidence_verification(
    workspace_override: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let snapshot = require_latest_run_evidence(workspace_override)?;
    if json {
        print_json(&snapshot.workflow)?;
    } else {
        println!("{}", snapshot.render_verification());
    }
    Ok(())
}

pub(crate) fn run_evidence_inspect(workspace_override: Option<PathBuf>, json: bool) -> Result<()> {
    let snapshot = require_latest_run_evidence(workspace_override)?;
    print_evidence_view(&snapshot, json, |snapshot| snapshot.render_inspect())
}

pub(crate) fn run_evidence_resume(mut params: CliParams, confirm: bool) -> Result<()> {
    let workspace = resolve_workspace_path(params.workspace.clone())?;
    let snapshot = RunEvidenceStore::new(workspace.clone()).require_latest()?;
    let freshness = snapshot.assess_freshness();

    if !snapshot.resume_hint.resumable {
        println!("{}", snapshot.render_status());
        bail!(
            "latest run evidence is {}; there is no unfinished run to resume",
            snapshot.status.label()
        );
    }

    let confirmation_required =
        snapshot.resume_hint.requires_operator_confirmation || freshness.stale_risk;
    if confirmation_required && !confirm {
        println!("{}", render_resume_refusal(&snapshot, &freshness));
        bail!("resume requires --confirm because the latest evidence has approval, mutation, external-send, or stale-workspace risk");
    }

    let resume_prompt = snapshot.build_resume_prompt(&freshness);
    if resume_prompt.chars().count() > MAX_RESUME_PROMPT_CHARS {
        bail!(
            "resume prompt exceeded {} chars; inspect latest run evidence before resuming",
            MAX_RESUME_PROMPT_CHARS
        );
    }

    params.workspace = Some(workspace);
    crate::commands::oneshot::run_one_shot(params, resume_prompt)
}

fn require_latest_run_evidence(workspace_override: Option<PathBuf>) -> Result<RunEvidenceSnapshot> {
    let workspace = resolve_workspace_path(workspace_override)?;
    Ok(RunEvidenceStore::new(workspace).require_latest()?)
}

fn print_evidence_view(
    snapshot: &RunEvidenceSnapshot,
    json: bool,
    render: impl FnOnce(&RunEvidenceSnapshot) -> String,
) -> Result<()> {
    if json {
        print_json(snapshot)?;
    } else {
        println!("{}", render(snapshot));
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
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

    let evidence_store = RunEvidenceStore::new(workspace.to_path_buf());
    match evidence_store.load_latest() {
        Ok(Some(snapshot)) => {
            let freshness = snapshot.assess_freshness();
            out.push_str("\nRun evidence:          present\n");
            out.push_str(&format!("  Run:                {}\n", snapshot.run_id));
            out.push_str(&format!(
                "  Status:             {}\n",
                snapshot.status.label()
            ));
            if let Some(phase) = &snapshot.phase {
                out.push_str(&format!("  Phase:              {}\n", phase));
            }
            if let Some(queue) = &snapshot.queue_status {
                out.push_str(&format!("  Queue:              {}\n", queue));
            }
            out.push_str(&format!(
                "  Freshness:          {}\n",
                freshness.short_label()
            ));
            out.push_str(&format!(
                "  Resumable:          {}\n",
                if snapshot.resume_hint.resumable {
                    "yes"
                } else {
                    "no"
                }
            ));
            if let Some(next) = &snapshot.resume_hint.next_safe_action {
                out.push_str(&format!("  Next safe action:   {}\n", next));
            }
        }
        Ok(None) => {
            out.push_str("\nRun evidence:          none\n");
        }
        Err(err) => {
            out.push_str(&format!("\nRun evidence:          error — {}\n", err));
        }
    }

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
        "\nNote: In-flight chat/task handles are not persisted. Latest run evidence is compact typed state, not raw transcript or full tool output. For service logs:\n  \
         journalctl --user -u topagent-telegram.service -n 50\n",
    );

    out
}

fn render_resume_refusal(
    snapshot: &RunEvidenceSnapshot,
    freshness: &RunEvidenceFreshness,
) -> String {
    let mut out = String::from("Resume needs operator confirmation.\n\n");
    out.push_str(&snapshot.render_status());
    out.push_str("\n\nFreshness:\n");
    out.push_str(&freshness.render());
    if let Some(reason) = &snapshot.resume_hint.reason {
        out.push_str("\n\nRisk:\n- ");
        out.push_str(reason);
    }
    out.push_str("\n\nRun `topagent run inspect` first, then `topagent run resume --confirm` if the continuity risk is acceptable.");
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
    use topagent_core::{
        RunCheckpoint, TaskResult, ToolActionOutcome, ToolActionReceipt, VerificationCommand,
    };

    fn sample_evidence_snapshot(
        workspace: &Path,
        run_id: &str,
        result: &TaskResult,
    ) -> RunEvidenceSnapshot {
        RunEvidenceSnapshot::from_task_result(
            workspace,
            run_id,
            RunCheckpoint {
                objective: Some("resume evidence test".to_string()),
                phase: Some("Verify".to_string()),
                queue_status: Some("1/2 done, 1 pending, 0 active, 0 blocked".to_string()),
                changed_files: result.files_changed().to_vec(),
                files_inspected: vec!["src/lib.rs".to_string()],
                unresolved_workflow_evidence_gaps: vec![
                    "missing final relevant passing verification".to_string(),
                ],
                next_required_evidence_action: Some("rerun cargo test".to_string()),
                ..RunCheckpoint::default()
            },
            None,
            result,
        )
    }

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

    #[test]
    fn test_run_status_includes_latest_evidence_without_raw_transcript() {
        let temp = tempfile::TempDir::new().unwrap();
        let result = TaskResult::new("fixed".to_string())
            .with_files_changed(vec!["src/lib.rs".to_string()])
            .with_verification_command(VerificationCommand {
                command: "cargo test".to_string(),
                output: "RAW SUCCESS LOG SHOULD NOT BE IN STATUS".to_string(),
                exit_code: 0,
                succeeded: true,
            });
        let snapshot = sample_evidence_snapshot(temp.path(), "run-status-test", &result);
        RunEvidenceStore::new(temp.path())
            .write_latest(&snapshot)
            .unwrap();

        let rendered = render_session_status(temp.path());

        assert!(rendered.contains("Run evidence:          present"));
        assert!(rendered.contains("run-status-test"));
        assert!(rendered.contains("Next safe action"));
        assert!(!rendered.contains("RAW SUCCESS LOG"));
        assert!(!rendered.contains("telegram-history"));
    }

    #[test]
    fn test_resume_refusal_shows_confirmation_reason_for_blocked_action() {
        let temp = tempfile::TempDir::new().unwrap();
        let result =
            TaskResult::new("blocked".to_string()).with_tool_receipt(ToolActionReceipt::new(
                "external_send",
                "patch",
                false,
                ToolActionOutcome::Blocked,
                "approval required: external_send upload",
            ));
        let snapshot = sample_evidence_snapshot(temp.path(), "run-blocked", &result);
        let freshness = snapshot.assess_freshness();

        let rendered = render_resume_refusal(&snapshot, &freshness);

        assert!(rendered.contains("Resume needs operator confirmation"));
        assert!(rendered.contains("external_send"));
        assert!(rendered.contains("topagent run resume --confirm"));
    }
}

use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, ApprovalMailbox, ApprovalMailboxMode, ApprovalRequest,
    CancellationToken, ProgressCallback, ProgressUpdate, WorkspaceCheckpointStore,
};
use tracing::{error, info, warn};

use crate::config::{load_persisted_telegram_defaults, resolve_one_shot_config, CliParams};
use crate::memory::{promote_verified_task, PromotionContext};
use crate::progress::LiveProgress;
use crate::run_setup::{build_agent, prepare_run_context, prepare_workspace_memory};

pub(crate) fn run_one_shot(params: CliParams, instruction: String) -> Result<()> {
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let config = resolve_one_shot_config(params, persisted_defaults)?;
    let workspace = config.workspace;
    let route = config.route;
    let api_key = config.api_key;
    let options = config.options;
    let configured_default_model = config.configured_default_model;

    let cancel_token = CancellationToken::new();
    let interactive_approvals = io::stdin().is_terminal() && io::stderr().is_terminal();
    let approval_mailbox = build_cli_approval_mailbox(interactive_approvals);
    let mut ctx = ExecutionContext::new(workspace.clone())
        .with_cancel_token(cancel_token.clone())
        .with_approval_mailbox(approval_mailbox)
        .with_workspace_checkpoint_store(WorkspaceCheckpointStore::new(workspace));
    let workspace_memory = prepare_workspace_memory(ctx.workspace_root.clone());
    let prepared_run = prepare_run_context(&ctx, &workspace_memory, &instruction, None);
    let loaded_procedure_files = prepared_run.loaded_procedure_files.clone();
    ctx = prepared_run.run_ctx;

    if configured_default_model != route.model_id {
        info!(
            "--model override active; configured default: {}",
            configured_default_model
        );
    }
    info!(
        "starting one-shot run | model: {} | workspace: {}",
        route.model_id,
        ctx.workspace_root.display()
    );
    info!("instruction: {}", instruction);

    let heartbeat_interval = Duration::from_secs(options.progress_heartbeat_secs);
    let distill_options = options.clone();
    let mut agent = build_agent(&route, &api_key, options);
    let progress = LiveProgress::for_cli(heartbeat_interval);
    let progress_callback = progress.callback();
    install_ctrlc_handler(cancel_token, progress_callback.clone())?;
    agent.set_progress_callback(Some(progress_callback));
    let result = agent.run(&ctx, &instruction);
    agent.set_progress_callback(None);
    progress.wait();

    match result {
        Ok(result) => {
            let mut final_output = result;
            if let Some(task_result) = agent.last_task_result().cloned() {
                match agent.plan().lock() {
                    Ok(plan) => match promote_verified_task(&PromotionContext {
                        memory: &workspace_memory,
                        ctx: &ctx,
                        options: &distill_options,
                        instruction: &instruction,
                        task_mode: agent.task_mode(),
                        task_result: &task_result,
                        plan: &plan.clone(),
                        durable_memory_written: agent.durable_memory_written_this_run(),
                        loaded_procedure_files: &loaded_procedure_files,
                    }) {
                        Ok(report) => {
                            if report.lesson_file.is_some()
                                || report.procedure_file.is_some()
                                || report.trajectory_file.is_some()
                            {
                                info!(
                                    lesson = report.lesson_file.as_deref().unwrap_or(""),
                                    procedure = report.procedure_file.as_deref().unwrap_or(""),
                                    trajectory = report.trajectory_file.as_deref().unwrap_or(""),
                                    "saved promoted workspace learning artifacts"
                                );
                            }
                            if !report.notes.is_empty() {
                                final_output.push_str("\n\n### Trust Notes\n");
                                for note in report.notes {
                                    final_output.push_str(&format!("- {}\n", note));
                                }
                            }
                        }
                        Err(err) => warn!("failed to promote verified task memory: {}", err),
                    },
                    Err(err) => warn!("failed to lock agent plan for distillation: {}", err),
                }
            }
            println!("{}", final_output);
            Ok(())
        }
        Err(topagent_core::Error::Stopped(_)) => {
            info!("one-shot run stopped by user");
            std::process::exit(130);
        }
        Err(topagent_core::Error::ApprovalRequired(request)) => {
            error!("approval required during one-shot run: {}", request);
            eprintln!(
                "{}",
                format_cli_approval_required(&request, interactive_approvals)
            );
            std::process::exit(2);
        }
        Err(e) => {
            error!("agent error: {}", e);
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn build_cli_approval_mailbox(interactive: bool) -> ApprovalMailbox {
    let mode = if interactive {
        ApprovalMailboxMode::Wait
    } else {
        ApprovalMailboxMode::Immediate
    };
    let mailbox = ApprovalMailbox::new(mode);
    if interactive {
        let mailbox_for_prompt = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            let stdin = io::stdin();
            let mut stderr = io::stderr();
            let decision =
                prompt_for_cli_approval_with_io(&request, &mut stdin.lock(), &mut stderr)
                    .unwrap_or(false);
            let result = if decision {
                mailbox_for_prompt.approve(&request.id, Some("approved in one-shot CLI".into()))
            } else {
                mailbox_for_prompt.deny(&request.id, Some("denied in one-shot CLI".into()))
            };
            if let Err(err) = result {
                let _ = writeln!(
                    stderr,
                    "failed to resolve approval request {}: {}",
                    request.id, err
                );
            }
        }));
    }
    mailbox
}

pub(crate) fn prompt_for_cli_approval_with_io(
    request: &ApprovalRequest,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<bool> {
    writeln!(writer, "\n{}\n", request.render_details())?;
    loop {
        write!(writer, "Approve this action? [y/N]: ")?;
        writer.flush()?;

        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            writeln!(writer)?;
            return Ok(false);
        }

        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => {
                writeln!(writer, "Please answer yes or no.")?;
            }
        }
    }
}

pub(crate) fn format_cli_approval_required(request: &ApprovalRequest, interactive: bool) -> String {
    let mut message = request.render_details();
    if interactive {
        message.push_str(
            "\n\nThe operator declined or did not resolve the approval in this one-shot run.",
        );
    } else {
        message.push_str(
            "\n\nThis one-shot run is non-interactive, so the action was not executed. Re-run from an interactive terminal to approve it.",
        );
    }
    message
}

fn install_ctrlc_handler(
    cancel_token: CancellationToken,
    progress_callback: ProgressCallback,
) -> Result<()> {
    let interrupt_count = Arc::new(AtomicUsize::new(0));
    ctrlc::set_handler(move || {
        let count = interrupt_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            cancel_token.cancel();
            progress_callback(ProgressUpdate::stopping());
        } else {
            eprintln!("status: forcing exit");
            std::process::exit(130);
        }
    })
    .context("Failed to install Ctrl-C handler")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use topagent_core::ApprovalTriggerKind;

    fn sample_request() -> ApprovalRequest {
        ApprovalRequest {
            id: "apr-7".to_string(),
            action_kind: ApprovalTriggerKind::GitCommit,
            short_summary: "git commit: ship it".to_string(),
            exact_action: "git_commit(message=\"ship it\")".to_string(),
            reason: "commits publish a durable repo milestone".to_string(),
            scope_of_impact: "Creates a new git commit in the workspace repository.".to_string(),
            expected_effect: "Staged changes become a durable repo milestone.".to_string(),
            rollback_hint: Some("Use git revert or git reset if the commit was mistaken.".into()),
            created_at: std::time::SystemTime::now(),
        }
    }

    #[test]
    fn test_prompt_for_cli_approval_accepts_yes() {
        let request = sample_request();
        let mut reader = Cursor::new(b"yes\n".to_vec());
        let mut output = Vec::new();

        let approved =
            prompt_for_cli_approval_with_io(&request, &mut reader, &mut output).unwrap();

        assert!(approved);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("Approve this action?"));
    }

    #[test]
    fn test_prompt_for_cli_approval_defaults_to_no_on_blank() {
        let request = sample_request();
        let mut reader = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();

        let approved =
            prompt_for_cli_approval_with_io(&request, &mut reader, &mut output).unwrap();

        assert!(!approved);
    }

    #[test]
    fn test_format_cli_approval_required_mentions_non_interactive_mode() {
        let request = sample_request();
        let message = format_cli_approval_required(&request, false);

        assert!(message.contains("non-interactive"));
        assert!(message.contains("apr-7"));
    }
}

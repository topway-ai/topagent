use anyhow::{Context, Result};
use topagent_core::context::ToolContext;
use topagent_core::tools::{SaveNoteArgs, SaveNoteTool, Tool};
use topagent_core::{
    DurablePromotionKind, ExecutionContext, Plan, RuntimeOptions, TaskMode, TaskResult,
};

use super::procedures::{
    evaluate_procedure_revision, find_matching_active_procedure, find_matching_loaded_procedure,
    mark_procedure_superseded, procedure_revision_quality_gate, record_procedure_reuse,
    revise_procedure, save_procedure, set_procedure_source_trajectory, ProcedureDraft,
    ProcedureRevisionAction,
};
use super::trajectories::{save_trajectory, TrajectoryDraft};
use super::{compact_text_line, memory_contract, WorkspaceMemory};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskPromotionReport {
    pub note_file: Option<String>,
    pub procedure_file: Option<String>,
    pub superseded_procedure_file: Option<String>,
    pub trajectory_file: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PromotionDecision {
    note: bool,
    procedure: bool,
    trajectory: bool,
}

impl PromotionDecision {
    fn is_empty(&self) -> bool {
        !self.note && !self.procedure && !self.trajectory
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PromotionContext<'a> {
    pub(crate) memory: &'a WorkspaceMemory,
    pub(crate) ctx: &'a ExecutionContext,
    pub(crate) options: &'a RuntimeOptions,
    pub(crate) instruction: &'a str,
    pub(crate) task_mode: TaskMode,
    pub(crate) task_result: &'a TaskResult,
    pub(crate) plan: &'a Plan,
    pub(crate) durable_memory_written: bool,
    pub(crate) loaded_procedure_files: &'a [String],
}

pub(crate) fn promote_verified_task(pc: &PromotionContext) -> Result<TaskPromotionReport> {
    let PromotionContext {
        memory,
        ctx,
        options,
        instruction,
        task_mode,
        task_result,
        plan,
        durable_memory_written,
        loaded_procedure_files,
    } = *pc;
    let decision = evaluate_promotion(instruction, task_result, plan, durable_memory_written);
    if decision.is_empty() {
        return Ok(TaskPromotionReport::default());
    }

    memory.ensure_layout()?;

    let stored_instruction = redact_for_storage(ctx, &normalize_task_text(instruction));
    let stored_task_result = redact_task_result_for_storage(ctx, task_result);
    let trust_context = stored_task_result.trust_context();
    let corroborated_by_trusted_local = stored_task_result.has_files_changed()
        && stored_task_result.final_verification_passed()
        && !stored_task_result.has_unresolved_issues();
    let contract = memory_contract();
    let tool_ctx = ToolContext::new(ctx, options);
    let mut report = TaskPromotionReport::default();

    if decision.note {
        if let Some(reason) = contract.durable_promotion_block_reason(
            DurablePromotionKind::Note,
            &trust_context,
            corroborated_by_trusted_local,
        ) {
            report.notes.push(reason);
        } else {
            let note_output = SaveNoteTool::new()
                .execute(
                    serde_json::to_value(build_note_args(
                        &stored_instruction,
                        &stored_task_result,
                    )?)
                    .context("failed to serialize note args")?,
                    &tool_ctx,
                )
                .map_err(anyhow::Error::new)?;
            report.note_file = extract_saved_artifact_path(&note_output, "Note saved to ");
        }
    }

    if decision.procedure {
        if let Some(reason) = contract.durable_promotion_block_reason(
            DurablePromotionKind::Procedure,
            &trust_context,
            corroborated_by_trusted_local,
        ) {
            report.notes.push(reason);
        } else {
            let reused =
                find_matching_loaded_procedure(memory, instruction, loaded_procedure_files)?;
            let procedure_draft = build_procedure_draft(
                &stored_instruction,
                &stored_task_result,
                plan,
                report.note_file.as_deref(),
                None,
                reused
                    .as_ref()
                    .map(|procedure| format!(".topagent/procedures/{}", procedure.filename)),
            )?;
            match reused {
                Some(existing) => {
                    let raw_action = evaluate_procedure_revision(&existing, &procedure_draft);
                    let action = procedure_revision_quality_gate(
                        &existing,
                        raw_action,
                        trust_context.has_low_trust_action_influence(),
                    );
                    if action != raw_action {
                        report.notes.push(format!(
                            "Procedure revision gate changed {} to {}: reuse {} | {}",
                            match raw_action {
                                ProcedureRevisionAction::Keep => "keep",
                                ProcedureRevisionAction::Refine => "refine",
                                ProcedureRevisionAction::Supersede => "supersede",
                            },
                            match action {
                                ProcedureRevisionAction::Keep => "keep",
                                ProcedureRevisionAction::Refine => "refine",
                                ProcedureRevisionAction::Supersede => "supersede",
                            },
                            existing.reuse_count,
                            if trust_context.has_low_trust_action_influence() {
                                "low-trust influence"
                            } else {
                                "below threshold"
                            },
                        ));
                    }
                    match action {
                        ProcedureRevisionAction::Keep => {
                            report.procedure_file = record_procedure_reuse(&existing.path, None)?
                                .or(Some(format!(".topagent/procedures/{}", existing.filename)));
                        }
                        ProcedureRevisionAction::Refine => {
                            report.procedure_file = revise_procedure(
                                &existing.path,
                                &procedure_draft,
                                report.note_file.as_deref(),
                                None,
                            )?
                            .or(Some(format!(".topagent/procedures/{}", existing.filename)));
                        }
                        ProcedureRevisionAction::Supersede => {
                            let (procedure_file, _path) =
                                save_procedure(&memory.procedures_dir, &procedure_draft)?;
                            report.procedure_file = Some(procedure_file.clone());
                            report.superseded_procedure_file =
                                mark_procedure_superseded(&existing.path, &procedure_file)?;
                        }
                    }
                }
                None => {
                    if let Some(existing) = find_matching_active_procedure(memory, instruction)? {
                        report.procedure_file =
                            Some(format!(".topagent/procedures/{}", existing.filename));
                    } else {
                        let (procedure_file, _path) =
                            save_procedure(&memory.procedures_dir, &procedure_draft)?;
                        report.procedure_file = Some(procedure_file);
                    }
                }
            }
        }
    }

    if decision.trajectory {
        let trajectory_draft = TrajectoryDraft {
            task_intent: compact_text_line(&stored_instruction, 220),
            task_mode,
            plan_summary: summarize_plan_items(plan),
            tool_sequence: stored_task_result.tool_trace().to_vec(),
            changed_files: stored_task_result.files_changed().to_vec(),
            verification: stored_task_result.verification_commands().to_vec(),
            outcome_summary: compact_text_line(&stored_task_result.outcome_summary, 220),
            note_file: report.note_file.clone(),
            procedure_file: report.procedure_file.clone(),
            source_labels: stored_task_result.source_labels().to_vec(),
        };
        let (trajectory_file, _path) =
            save_trajectory(&memory.trajectories_dir, &trajectory_draft)?;
        report.trajectory_file = Some(trajectory_file.clone());
        if let Some(summary) = trust_context.low_trust_action_summary(2) {
            report.notes.push(format!(
                "Trajectory saved locally with low-trust provenance from {}. Review and export stay blocked until trusted corroboration is established.",
                summary
            ));
        }
        if let Some(procedure_file) = report.procedure_file.clone() {
            if let Some(filename) = super::artifact_filename(&procedure_file) {
                set_procedure_source_trajectory(
                    &memory.procedures_dir.join(filename),
                    &trajectory_file,
                )?;
            }
        }
    }

    if report.note_file.is_some()
        || report.procedure_file.is_some()
        || report.trajectory_file.is_some()
    {
        memory.consolidate_memory_if_needed()?;
    }

    Ok(report)
}

fn normalize_task_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn redact_for_storage(ctx: &ExecutionContext, text: &str) -> String {
    ctx.secrets().redact(text).into_owned()
}

fn redact_task_result_for_storage(ctx: &ExecutionContext, task_result: &TaskResult) -> TaskResult {
    let mut redacted = task_result.clone();
    redacted.outcome_summary = redact_for_storage(ctx, &redacted.outcome_summary);
    redacted.evidence.diff_summary = redact_for_storage(ctx, &redacted.evidence.diff_summary);
    redacted.evidence.unresolved_issues = redacted
        .evidence
        .unresolved_issues
        .iter()
        .map(|issue| redact_for_storage(ctx, issue))
        .collect();
    redacted.evidence.tool_trace = redacted
        .evidence
        .tool_trace
        .iter()
        .map(|step| topagent_core::ToolTraceStep {
            tool_name: step.tool_name.clone(),
            summary: redact_for_storage(ctx, &step.summary),
        })
        .collect();
    redacted.evidence.verification_commands_run = redacted
        .evidence
        .verification_commands_run
        .iter()
        .map(|command| topagent_core::VerificationCommand {
            command: redact_for_storage(ctx, &command.command),
            output: String::new(),
            exit_code: command.exit_code,
            succeeded: command.succeeded,
        })
        .collect();
    redacted.evidence.source_labels = redacted
        .evidence
        .source_labels
        .iter()
        .map(|label| {
            topagent_core::SourceLabel::new(
                label.kind,
                label.trust,
                label.influence,
                redact_for_storage(ctx, &label.summary),
            )
        })
        .collect();
    redacted
}

fn summarize_changed_files(files: &[String]) -> String {
    match files {
        [] => "the verified workspace changes".to_string(),
        [file] => format!("`{file}`"),
        [first, second] => format!("`{first}` and `{second}`"),
        [first, ..] => format!("`{first}` and {} more files", files.len() - 1),
    }
}

fn build_distilled_what_changed(outcome_summary: &str, changed_files: &str) -> String {
    let summary = compact_text_line(&normalize_task_text(outcome_summary), 220);
    if summary.is_empty() {
        format!("Verified work touched {changed_files}.")
    } else {
        format!("{summary} Verified work touched {changed_files}.")
    }
}

fn build_distilled_what_learned(
    changed_files: &str,
    verification_command: Option<&str>,
    had_failed_verification: bool,
    touched_multiple_files: bool,
) -> String {
    if had_failed_verification {
        return match verification_command {
            Some(command) => format!(
                "The task was only complete after rerunning `{command}` to a passing result; keep that final pass as the completion gate."
            ),
            None => {
                "The task was only complete after rerunning the verification to a passing result."
                    .to_string()
            }
        };
    }

    if touched_multiple_files {
        return format!(
            "Related edits across {changed_files} need to stay synchronized before the final verification pass."
        );
    }

    match verification_command {
        Some(command) => format!(
            "Use `{command}` as the completion check for similar changes in {changed_files}."
        ),
        None => format!(
            "For similar work, keep the edits in {changed_files} tied to an explicit passing verification step."
        ),
    }
}

fn build_note_title(instruction: &str, task_result: &TaskResult) -> String {
    let changed_files = summarize_changed_files(task_result.files_changed());
    if task_result
        .verification_commands()
        .iter()
        .any(|command| !command.succeeded)
    {
        if let Some(command) = task_result.latest_verification_command() {
            return compact_text_line(
                &format!(
                    "Finish {} only after `{}` passes",
                    changed_files,
                    compact_text_line(&command.command, 48)
                ),
                72,
            );
        }
    }

    if task_result.files_changed().len() >= 2 {
        return compact_text_line(
            &format!("Keep related edits synchronized across {changed_files}"),
            72,
        );
    }

    if let Some(command) = task_result.latest_verification_command() {
        return compact_text_line(
            &format!(
                "Use `{}` as the completion check for {}",
                compact_text_line(&command.command, 48),
                changed_files
            ),
            72,
        );
    }

    compact_text_line(&normalize_task_text(instruction), 72)
}

fn evaluate_promotion(
    instruction: &str,
    task_result: &TaskResult,
    plan: &Plan,
    durable_memory_written: bool,
) -> PromotionDecision {
    if durable_memory_written
        || instruction.trim().is_empty()
        || !task_result.final_verification_passed()
        || !task_result.has_files_changed()
        || task_result.has_unresolved_issues()
        || task_result.verification_commands().is_empty()
    {
        return PromotionDecision::default();
    }

    let multi_step_plan = plan.items().len() >= 2;
    let multi_file = task_result.files_changed().len() >= 2;
    let repeated_verification = task_result.verification_commands().len() >= 2;
    let note = multi_step_plan || multi_file || repeated_verification;
    let procedure = note && multi_step_plan && task_result.tool_trace().len() >= 3;
    let trajectory = procedure && (plan.items().len() >= 3 || multi_file || repeated_verification);

    PromotionDecision {
        note,
        procedure,
        trajectory,
    }
}

fn build_note_args(instruction: &str, task_result: &TaskResult) -> Result<SaveNoteArgs> {
    let note_title = build_note_title(instruction, task_result);
    if note_title.is_empty() {
        return Err(anyhow::anyhow!("note title is empty after normalization"));
    }

    let changed_files = summarize_changed_files(task_result.files_changed());
    let verification_command = task_result
        .latest_verification_command()
        .map(|command| compact_text_line(&command.command, 96));
    let avoid_next_time = task_result
        .verification_commands()
        .iter()
        .rfind(|command| !command.succeeded)
        .map(|command| {
            format!(
                "Do not stop after `{}` fails; rerun the final verification until it passes.",
                compact_text_line(&command.command, 96)
            )
        });
    let had_failed_verification = task_result
        .verification_commands()
        .iter()
        .any(|command| !command.succeeded);

    Ok(SaveNoteArgs {
        title: note_title,
        what_changed: build_distilled_what_changed(&task_result.outcome_summary, &changed_files),
        what_learned: build_distilled_what_learned(
            &changed_files,
            verification_command.as_deref(),
            had_failed_verification,
            task_result.files_changed().len() >= 2,
        ),
        reuse_next_time: verification_command
            .map(|command| format!("Reuse `{command}` as the completion check for similar edits.")),
        avoid_next_time,
    })
}

fn build_procedure_draft(
    instruction: &str,
    task_result: &TaskResult,
    plan: &Plan,
    source_note: Option<&str>,
    source_trajectory: Option<&str>,
    supersedes: Option<String>,
) -> Result<ProcedureDraft> {
    let title = compact_text_line(&normalize_task_text(instruction), 80);
    if title.is_empty() {
        return Err(anyhow::anyhow!(
            "procedure title is empty after normalization"
        ));
    }

    let verification = task_result
        .latest_verification_command()
        .map(|command| compact_text_line(&command.command, 120))
        .unwrap_or_else(|| "Use the repo's current passing verification command.".to_string());

    let steps = if !plan.is_empty() {
        plan.items()
            .iter()
            .take(6)
            .map(|item| compact_text_line(&item.description, 120))
            .collect::<Vec<_>>()
    } else {
        vec![
            format!(
                "Inspect the current implementation around {}.",
                summarize_changed_files(task_result.files_changed())
            ),
            format!(
                "Apply the requested changes in {}.",
                summarize_changed_files(task_result.files_changed())
            ),
            format!("Run `{verification}` and confirm it passes."),
        ]
    };

    let mut pitfalls = Vec::new();
    if task_result
        .verification_commands()
        .iter()
        .any(|command| !command.succeeded)
    {
        pitfalls.push(format!(
            "Do not stop at the first failing verification; finish only after `{verification}` passes."
        ));
    }
    if task_result.files_changed().len() >= 2 {
        pitfalls.push(format!(
            "Keep related edits synchronized across {}.",
            summarize_changed_files(task_result.files_changed())
        ));
    }

    Ok(ProcedureDraft {
        title,
        when_to_use: format!(
            "Use when similar repo work needs to change {} and finish with `{}`.",
            summarize_changed_files(task_result.files_changed()),
            verification
        ),
        prerequisites: vec![
            "Stay within the current workspace and preserve the existing operator-approved workflow."
                .to_string(),
        ],
        steps,
        pitfalls,
        verification,
        source_task: Some(compact_text_line(&normalize_task_text(instruction), 160)),
        source_note: source_note.map(ToString::to_string),
        source_trajectory: source_trajectory.map(ToString::to_string),
        supersedes,
    })
}

fn summarize_plan_items(plan: &Plan) -> Vec<String> {
    plan.items()
        .iter()
        .take(6)
        .map(|item| compact_text_line(&item.description, 120))
        .collect()
}

fn extract_saved_artifact_path(output: &str, prefix: &str) -> Option<String> {
    output
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(prefix))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod scenario_tests {
    use crate::memory::procedures::{
        evaluate_procedure_revision, procedure_revision_quality_gate, ParsedProcedure,
        ProcedureDraft, ProcedureRevisionAction, ProcedureStatus,
    };
    use std::path::PathBuf;

    fn procedure_with_reuse(reuse_count: u32) -> ParsedProcedure {
        ParsedProcedure {
            filename: "100-deploy-workflow.md".to_string(),
            title: "Deploy workflow".to_string(),
            saved_at: Some(100),
            status: ProcedureStatus::Active,
            when_to_use: "Use for deployment.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec!["Deploy.".to_string(), "Verify.".to_string()],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test".to_string(),
            reuse_count,
            revision_count: 0,
            last_verified_reuse_at: None,
            source_task: None,
            source_note: None,
            source_trajectory: None,
            supersedes: None,
            superseded_by: None,
            disabled_reason: None,
            path: PathBuf::from(".topagent/procedures/100-deploy-workflow.md"),
        }
    }

    fn draft_with_new_steps() -> ProcedureDraft {
        ProcedureDraft {
            title: "Deploy workflow".to_string(),
            when_to_use: "Use for deployment.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Deploy.".to_string(),
                "Verify.".to_string(),
                "Notify team.".to_string(),
            ],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test".to_string(),
            source_task: Some("deploy".to_string()),
            source_note: None,
            source_trajectory: None,
            supersedes: None,
        }
    }

    #[test]
    fn test_gate_downgrade_refine_to_keep_reason_text() {
        let existing = procedure_with_reuse(1);
        let draft = draft_with_new_steps();
        let raw_action = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw_action, ProcedureRevisionAction::Refine);
        let gated_action = procedure_revision_quality_gate(&existing, raw_action, false);
        assert_eq!(gated_action, ProcedureRevisionAction::Keep);

        let reason = if existing.reuse_count < 3 {
            "below threshold"
        } else {
            ""
        };
        assert_eq!(reason, "below threshold");
        let note = format!(
            "Procedure revision gate changed {} to {}: reuse {} | {}",
            "refine", "keep", existing.reuse_count, reason
        );
        assert!(note.contains("below threshold"));
        assert!(note.contains("reuse 1"));
    }

    #[test]
    fn test_gate_downgrade_supersede_to_keep_reason_text() {
        let existing = procedure_with_reuse(1);
        let draft = ProcedureDraft {
            title: "Deploy workflow".to_string(),
            when_to_use: "Use for deployment.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec!["Deploy differently.".to_string()],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test -p other".to_string(),
            source_task: None,
            source_note: None,
            source_trajectory: None,
            supersedes: None,
        };
        let raw_action = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw_action, ProcedureRevisionAction::Supersede);
        let gated_action = procedure_revision_quality_gate(&existing, raw_action, false);
        assert_eq!(gated_action, ProcedureRevisionAction::Keep);
        let reason = "below threshold";
        let note = format!(
            "Procedure revision gate changed {} to {}: reuse {} | {}",
            "supersede", "keep", existing.reuse_count, reason
        );
        assert!(note.contains("below threshold"));
    }

    #[test]
    fn test_gate_no_downgrade_means_no_note() {
        let existing = procedure_with_reuse(3);
        let draft = draft_with_new_steps();
        let raw_action = evaluate_procedure_revision(&existing, &draft);
        let gated_action = procedure_revision_quality_gate(&existing, raw_action, false);
        assert_eq!(raw_action, gated_action);
        assert_eq!(gated_action, ProcedureRevisionAction::Refine);
        let should_add_note = raw_action != gated_action;
        assert!(
            !should_add_note,
            "no note should be added when gate does not downgrade"
        );
    }

    #[test]
    fn test_gate_low_trust_always_downgrades_to_keep() {
        let existing = procedure_with_reuse(10);
        let draft = draft_with_new_steps();
        let raw_action = evaluate_procedure_revision(&existing, &draft);
        let gated_action = procedure_revision_quality_gate(&existing, raw_action, true);
        assert_eq!(gated_action, ProcedureRevisionAction::Keep);
        let reason = "low-trust influence";
        let note = format!(
            "Procedure revision gate changed {} to {}: reuse {} | {}",
            match raw_action {
                ProcedureRevisionAction::Keep => "keep",
                ProcedureRevisionAction::Refine => "refine",
                ProcedureRevisionAction::Supersede => "supersede",
            },
            "keep",
            existing.reuse_count,
            reason
        );
        assert!(note.contains("low-trust influence"));
    }

    #[test]
    fn test_promotion_no_auto_rewrite_on_keep() {
        let existing = procedure_with_reuse(0);
        let draft = draft_with_new_steps();
        let raw_action = evaluate_procedure_revision(&existing, &draft);
        let gated_action = procedure_revision_quality_gate(&existing, raw_action, false);
        assert_eq!(gated_action, ProcedureRevisionAction::Keep);
        assert_eq!(raw_action, ProcedureRevisionAction::Refine);
        assert_eq!(existing.revision_count, 0);
    }
}

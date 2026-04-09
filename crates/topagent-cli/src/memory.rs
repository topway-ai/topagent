mod memory_consolidation;
mod procedures;
mod trajectories;

use self::memory_consolidation::{
    parse_saved_lesson, parse_saved_plan, render_saved_lesson_excerpt, render_saved_plan_excerpt,
    MemoryIndexEntry,
};
use self::procedures::{
    mark_procedure_superseded, parse_saved_procedure, procedure_haystack,
    render_saved_procedure_excerpt, save_procedure, set_procedure_source_trajectory,
    ParsedProcedure, ProcedureDraft, ProcedureStatus,
};
use self::trajectories::{save_trajectory, TrajectoryDraft};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use topagent_core::context::ToolContext;
use topagent_core::tools::{SaveLessonArgs, SaveLessonTool, Tool};
use topagent_core::{
    BehaviorContract, ExecutionContext, Message, Plan, Role, RuntimeOptions, TaskMode, TaskResult,
};
use tracing::warn;

use crate::managed_files::write_managed_file;

const MEMORY_ROOT_DIR: &str = ".topagent";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_TOPICS_RELATIVE_DIR: &str = ".topagent/topics";
pub(crate) const MEMORY_LESSONS_RELATIVE_DIR: &str = ".topagent/lessons";
pub(crate) const MEMORY_PLANS_RELATIVE_DIR: &str = ".topagent/plans";
pub(crate) const MEMORY_PROCEDURES_RELATIVE_DIR: &str = ".topagent/procedures";
pub(crate) const MEMORY_TRAJECTORIES_RELATIVE_DIR: &str = ".topagent/trajectories";
const AUTO_PROMOTED_TAG: &str = "curated";

const STOP_WORDS: &[&str] = &[
    "and",
    "about",
    "after",
    "agent",
    "also",
    "are",
    "ask",
    "asked",
    "been",
    "before",
    "chat",
    "code",
    "did",
    "does",
    "file",
    "for",
    "from",
    "have",
    "into",
    "just",
    "last",
    "mention",
    "mentioned",
    "more",
    "need",
    "note",
    "only",
    "over",
    "please",
    "repo",
    "said",
    "same",
    "stored",
    "that",
    "the",
    "them",
    "then",
    "they",
    "this",
    "was",
    "what",
    "when",
    "were",
    "with",
    "work",
    "workspace",
    "would",
    "your",
];

fn memory_contract() -> BehaviorContract {
    BehaviorContract::default()
}

fn memory_index_template() -> String {
    memory_contract().render_memory_index_template()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MemoryPrompt {
    pub prompt: Option<String>,
    pub stats: MemoryPromptStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MemoryPromptStats {
    pub index_prompt_bytes: usize,
    pub loaded_items: Vec<String>,
    pub transcript_snippets: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMemory {
    workspace_root: PathBuf,
    index_path: PathBuf,
    topics_dir: PathBuf,
    lessons_dir: PathBuf,
    plans_dir: PathBuf,
    procedures_dir: PathBuf,
    trajectories_dir: PathBuf,
}

impl WorkspaceMemory {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self {
            index_path: workspace_root.join(MEMORY_INDEX_RELATIVE_PATH),
            topics_dir: workspace_root.join(MEMORY_TOPICS_RELATIVE_DIR),
            lessons_dir: workspace_root.join(MEMORY_LESSONS_RELATIVE_DIR),
            plans_dir: workspace_root.join(MEMORY_PLANS_RELATIVE_DIR),
            procedures_dir: workspace_root.join(MEMORY_PROCEDURES_RELATIVE_DIR),
            trajectories_dir: workspace_root.join(MEMORY_TRAJECTORIES_RELATIVE_DIR),
            workspace_root,
        }
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn ensure_layout(&self) -> Result<()> {
        std::fs::create_dir_all(&self.topics_dir)
            .with_context(|| format!("failed to create {}", self.topics_dir.display()))?;
        std::fs::create_dir_all(&self.lessons_dir)
            .with_context(|| format!("failed to create {}", self.lessons_dir.display()))?;
        std::fs::create_dir_all(&self.plans_dir)
            .with_context(|| format!("failed to create {}", self.plans_dir.display()))?;
        std::fs::create_dir_all(&self.procedures_dir)
            .with_context(|| format!("failed to create {}", self.procedures_dir.display()))?;
        std::fs::create_dir_all(&self.trajectories_dir)
            .with_context(|| format!("failed to create {}", self.trajectories_dir.display()))?;

        if !self.index_path.exists() {
            if let Some(parent) = self.index_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let template = memory_index_template();
            write_managed_file(&self.index_path, &template, false)?;
        }

        Ok(())
    }

    pub(crate) fn build_prompt(
        &self,
        instruction: &str,
        transcript_messages: Option<&[Message]>,
    ) -> Result<MemoryPrompt> {
        let contract = memory_contract();
        let entries = self.load_index_entries()?;
        let index_section = render_index_section(&contract, &entries);
        let procedure_load = self.render_procedure_section(&contract, instruction, &entries)?;
        let durable_load = self.render_durable_notes_section(&contract, instruction, &entries)?;
        let transcript_section = transcript_messages
            .and_then(|messages| render_transcript_section(&contract, instruction, messages));

        if index_section.is_none()
            && procedure_load.section.is_none()
            && durable_load.section.is_none()
            && transcript_section.is_none()
        {
            return Ok(MemoryPrompt::default());
        }

        let mut prompt = String::new();
        prompt.push_str(&contract.render_memory_prompt_preamble());

        let mut stats = MemoryPromptStats::default();

        if let Some(index_section) = index_section {
            stats.index_prompt_bytes = index_section.len();
            prompt.push_str("\n### Always-Loaded Index\n");
            prompt.push_str(&index_section);
        }

        if let Some(procedure_section) = procedure_load.section {
            stats.loaded_items.extend(procedure_load.loaded_items);
            prompt.push_str("\n### Relevant Procedures\n");
            prompt.push_str(&procedure_section);
        }

        if let Some(durable_section) = durable_load.section {
            stats.loaded_items.extend(durable_load.loaded_items);
            prompt.push_str("\n### Curated Durable Notes\n");
            prompt.push_str(&durable_section);
        }

        if let Some(transcript_section) = transcript_section {
            stats.transcript_snippets = transcript_section.snippet_count;
            prompt.push_str("\n### Transcript Evidence\n");
            prompt.push_str(&contract.render_memory_transcript_preamble());
            prompt.push_str(&transcript_section.section);
        }

        Ok(MemoryPrompt {
            prompt: Some(prompt.trim_end().to_string()),
            stats,
        })
    }

    fn render_procedure_section(
        &self,
        contract: &BehaviorContract,
        instruction: &str,
        entries: &[MemoryIndexEntry],
    ) -> Result<TopicLoad> {
        let mut scored_entries = entries
            .iter()
            .filter(|entry| is_procedure_entry(entry))
            .filter_map(|entry| {
                let score = score_entry_relevance(instruction, entry);
                (score > 0).then_some((score, entry))
            })
            .collect::<Vec<_>>();
        scored_entries.sort_by(|(left_score, left_entry), (right_score, right_entry)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_entry.topic.cmp(&right_entry.topic))
        });

        let mut section = String::new();
        let mut loaded_items = Vec::new();

        for (_, entry) in scored_entries
            .into_iter()
            .take(contract.memory.max_procedures_to_load)
        {
            let Some(path) = self.resolve_memory_path(contract, &entry.file) else {
                warn!(
                    "ignoring unsafe procedure path `{}` from {}",
                    entry.file,
                    self.index_path.display()
                );
                continue;
            };

            let Some(procedure) = parse_saved_procedure(&path)? else {
                continue;
            };
            if procedure.status == ProcedureStatus::Superseded {
                continue;
            }

            let excerpt = render_saved_procedure_excerpt(contract, &procedure);
            if excerpt.is_empty() {
                continue;
            }

            if !section.is_empty() {
                section.push('\n');
            }
            section.push_str(&format!(
                "[{}] {} ({})\n{}\n",
                entry.status,
                procedure.title,
                display_memory_file(&entry.file),
                excerpt
            ));
            loaded_items.push(procedure.title);
        }

        Ok(TopicLoad {
            section: (!section.is_empty()).then_some(section),
            loaded_items,
        })
    }

    fn render_durable_notes_section(
        &self,
        contract: &BehaviorContract,
        instruction: &str,
        entries: &[MemoryIndexEntry],
    ) -> Result<TopicLoad> {
        let mut scored_entries = entries
            .iter()
            .filter(|entry| !is_procedure_entry(entry))
            .filter_map(|entry| {
                let score = score_entry_relevance(instruction, entry);
                (score > 0).then_some((score, entry))
            })
            .collect::<Vec<_>>();
        scored_entries.sort_by(|(left_score, left_entry), (right_score, right_entry)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_entry.topic.cmp(&right_entry.topic))
        });

        let mut section = String::new();
        let mut loaded_items = Vec::new();

        for (_, entry) in scored_entries
            .into_iter()
            .take(contract.memory.max_topics_to_load)
        {
            let Some(path) = self.resolve_memory_path(contract, &entry.file) else {
                warn!(
                    "ignoring unsafe memory path `{}` from {}",
                    entry.file,
                    self.index_path.display()
                );
                continue;
            };

            if !path.exists() {
                continue;
            }

            let excerpt = self.render_memory_file_excerpt(contract, entry, &path)?;
            if excerpt.is_empty() {
                continue;
            }

            if !section.is_empty() {
                section.push('\n');
            }

            section.push_str(&format!(
                "[{}] {} ({})\n{}\n",
                entry.status,
                entry.topic,
                display_memory_file(&entry.file),
                excerpt
            ));
            loaded_items.push(entry.topic.clone());
        }

        Ok(TopicLoad {
            section: (!section.is_empty()).then_some(section),
            loaded_items,
        })
    }

    fn resolve_memory_path(&self, contract: &BehaviorContract, file: &str) -> Option<PathBuf> {
        let normalized = normalize_memory_file(file);
        let relative = if allowed_memory_prefix(contract, &normalized) {
            normalized
        } else {
            format!("{}/{}", contract.memory.topic_file_relative_dir, normalized)
        };

        let relative_path = Path::new(&relative);
        if relative_path.is_absolute() {
            return None;
        }

        for component in relative_path.components() {
            if matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return None;
            }
        }

        Some(
            self.workspace_root
                .join(MEMORY_ROOT_DIR)
                .join(relative_path),
        )
    }

    fn render_memory_file_excerpt(
        &self,
        contract: &BehaviorContract,
        entry: &MemoryIndexEntry,
        path: &Path,
    ) -> Result<String> {
        let display_path = display_memory_file(&entry.file);
        if display_path.starts_with("lessons/") {
            if let Some(parsed) = parse_saved_lesson(path)? {
                return Ok(render_saved_lesson_excerpt(contract, &parsed));
            }
        }
        if display_path.starts_with("plans/") {
            if let Some(parsed) = parse_saved_plan(path)? {
                return Ok(render_saved_plan_excerpt(contract, &parsed));
            }
        }
        if display_path.starts_with("procedures/") {
            if let Some(parsed) = parse_saved_procedure(path)? {
                return Ok(render_saved_procedure_excerpt(contract, &parsed));
            }
        }

        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(limit_text_block(
            &raw,
            contract.memory.max_durable_file_prompt_bytes,
        ))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TopicLoad {
    section: Option<String>,
    loaded_items: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskPromotionReport {
    pub lesson_file: Option<String>,
    pub procedure_file: Option<String>,
    pub superseded_procedure_file: Option<String>,
    pub trajectory_file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PromotionDecision {
    lesson: bool,
    procedure: bool,
    trajectory: bool,
}

impl PromotionDecision {
    fn is_empty(&self) -> bool {
        !self.lesson && !self.procedure && !self.trajectory
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TranscriptSection {
    section: String,
    snippet_count: usize,
}

pub(crate) fn promote_verified_task(
    memory: &WorkspaceMemory,
    ctx: &ExecutionContext,
    options: &RuntimeOptions,
    instruction: &str,
    task_mode: TaskMode,
    task_result: &TaskResult,
    plan: &Plan,
    durable_memory_written: bool,
) -> Result<TaskPromotionReport> {
    let decision = evaluate_promotion(instruction, task_result, plan, durable_memory_written);
    if decision.is_empty() {
        return Ok(TaskPromotionReport::default());
    }

    memory.ensure_layout()?;

    let stored_instruction = redact_for_storage(ctx, &normalize_task_text(instruction));
    let stored_task_result = redact_task_result_for_storage(ctx, task_result);
    let tool_ctx = ToolContext::new(ctx, options);
    let mut report = TaskPromotionReport::default();

    if decision.lesson {
        let lesson_output = SaveLessonTool::new()
            .execute(
                serde_json::to_value(build_lesson_args(&stored_instruction, &stored_task_result)?)
                    .context("failed to serialize lesson args")?,
                &tool_ctx,
            )
            .map_err(anyhow::Error::new)?;
        report.lesson_file = extract_saved_artifact_path(&lesson_output, "Lesson saved to ");
    }

    if decision.procedure {
        let matching = find_matching_active_procedure(memory, instruction)?;
        let procedure_draft = build_procedure_draft(
            &stored_instruction,
            &stored_task_result,
            plan,
            report.lesson_file.as_deref(),
            None,
            matching
                .as_ref()
                .map(|procedure| format!(".topagent/procedures/{}", procedure.filename)),
        )?;
        let (procedure_file, _path) = save_procedure(&memory.procedures_dir, &procedure_draft)?;
        report.procedure_file = Some(procedure_file.clone());
        if let Some(existing) = matching {
            report.superseded_procedure_file =
                mark_procedure_superseded(&existing.path, &procedure_file)?;
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
            lesson_file: report.lesson_file.clone(),
            procedure_file: report.procedure_file.clone(),
        };
        let (trajectory_file, _path) =
            save_trajectory(&memory.trajectories_dir, &trajectory_draft)?;
        report.trajectory_file = Some(trajectory_file.clone());
        if let Some(procedure_file) = report.procedure_file.clone() {
            if let Some(filename) = artifact_filename(&procedure_file) {
                set_procedure_source_trajectory(
                    &memory.procedures_dir.join(filename),
                    &trajectory_file,
                )?;
            }
        }
    }

    if report.lesson_file.is_some()
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
    redacted.evidence.workspace_warnings = redacted
        .evidence
        .workspace_warnings
        .iter()
        .map(|warning| redact_for_storage(ctx, warning))
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
            None => "The task was only complete after rerunning the verification to a passing result.".to_string(),
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

fn build_lesson_title(instruction: &str, task_result: &TaskResult) -> String {
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
    let lesson = multi_step_plan || multi_file || repeated_verification;
    let procedure = lesson && multi_step_plan && task_result.tool_trace().len() >= 3;
    let trajectory = procedure
        && (plan.items().len() >= 3 || multi_file || repeated_verification)
        && task_result.evidence.workspace_warnings.is_empty();

    PromotionDecision {
        lesson,
        procedure,
        trajectory,
    }
}

fn build_lesson_args(instruction: &str, task_result: &TaskResult) -> Result<SaveLessonArgs> {
    let lesson_title = build_lesson_title(instruction, task_result);
    if lesson_title.is_empty() {
        return Err(anyhow::anyhow!("lesson title is empty after normalization"));
    }

    let changed_files = summarize_changed_files(task_result.files_changed());
    let verification_command = task_result
        .latest_verification_command()
        .map(|command| compact_text_line(&command.command, 96));
    let avoid_next_time = task_result
        .verification_commands()
        .iter()
        .filter(|command| !command.succeeded)
        .last()
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

    Ok(SaveLessonArgs {
        title: lesson_title,
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
    source_lesson: Option<&str>,
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
        source_lesson: source_lesson.map(ToString::to_string),
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

fn find_matching_active_procedure(
    memory: &WorkspaceMemory,
    instruction: &str,
) -> Result<Option<ParsedProcedure>> {
    let mut best: Option<(usize, ParsedProcedure)> = None;
    for path in list_markdown_files(&memory.procedures_dir)? {
        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        if procedure.status == ProcedureStatus::Superseded {
            continue;
        }

        let score = score_text_relevance(instruction, &procedure_haystack(&procedure));
        if score < 4 {
            continue;
        }

        match &best {
            Some((best_score, _)) if *best_score >= score => {}
            _ => best = Some((score, procedure)),
        }
    }

    Ok(best.map(|(_, procedure)| procedure))
}

fn list_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
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

fn artifact_filename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|name| name.to_str())
}

fn render_index_section(
    contract: &BehaviorContract,
    entries: &[MemoryIndexEntry],
) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut section = String::new();
    let mut omitted = 0usize;

    for (idx, entry) in entries.iter().enumerate() {
        let mut line = format!(
            "- [{}] {} -> {}",
            entry.status,
            entry.topic,
            display_memory_file(&entry.file)
        );
        if !entry.note.is_empty() {
            line.push_str(" :: ");
            line.push_str(&entry.note);
        }
        line.push('\n');

        if section.len() + line.len() > contract.memory.max_index_prompt_bytes {
            omitted = entries.len().saturating_sub(idx);
            break;
        }
        section.push_str(&line);
    }

    if omitted > 0 {
        section.push_str(&format!(
            "- ... {} more index entries omitted to keep startup memory cheap.\n",
            omitted
        ));
    }

    Some(section)
}

fn render_transcript_section(
    contract: &BehaviorContract,
    instruction: &str,
    messages: &[Message],
) -> Option<TranscriptSection> {
    let transcript = messages
        .iter()
        .filter_map(|message| {
            let text = message.as_text()?;
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                _ => return None,
            };
            let compact = compact_text_line(text, contract.memory.max_transcript_message_bytes);
            (!compact.is_empty()).then_some((role.to_string(), compact))
        })
        .collect::<Vec<_>>();

    if transcript.is_empty() {
        return None;
    }

    let instruction_tokens = tokenize(instruction);
    let lower_instruction = instruction.to_ascii_lowercase();
    let recall_like = looks_like_recall_query(&lower_instruction);

    let mut windows = match_windows(&transcript, &instruction_tokens, recall_like);
    if windows.is_empty() && recall_like {
        let start = transcript.len().saturating_sub(4);
        windows.push((start, transcript.len() - 1));
    }
    if windows.is_empty() {
        return None;
    }

    let mut section = String::new();
    let mut snippet_count = 0usize;

    for (start, end) in windows
        .into_iter()
        .take(contract.memory.max_transcript_snippets)
    {
        let mut snippet = format!("Snippet {}:\n", snippet_count + 1);
        for (role, text) in transcript.iter().skip(start).take(end - start + 1) {
            snippet.push_str(&format!("{role}: {text}\n"));
        }

        if section.len() + snippet.len() > contract.memory.max_transcript_prompt_bytes {
            break;
        }

        section.push_str(&snippet);
        section.push('\n');
        snippet_count += 1;
    }

    (snippet_count > 0).then_some(TranscriptSection {
        section,
        snippet_count,
    })
}

fn match_windows(
    transcript: &[(String, String)],
    instruction_tokens: &HashSet<String>,
    recall_like: bool,
) -> Vec<(usize, usize)> {
    let mut scored = Vec::new();

    for (idx, (_, text)) in transcript.iter().enumerate() {
        let text_tokens = tokenize(text);
        let overlap = text_tokens.intersection(instruction_tokens).count();
        if overlap == 0 {
            continue;
        }

        let recency_bonus = if idx + 4 >= transcript.len() { 1 } else { 0 };
        let score = overlap * 3 + recency_bonus;
        scored.push((score, idx));
    }

    scored.sort_by(|(left_score, left_idx), (right_score, right_idx)| {
        right_score
            .cmp(left_score)
            .then_with(|| right_idx.cmp(left_idx))
    });

    let best_score = scored.first().map(|(score, _)| *score).unwrap_or(0);
    if !recall_like && best_score < 6 {
        return Vec::new();
    }
    if recall_like && best_score == 0 {
        return Vec::new();
    }

    let mut windows: Vec<(usize, usize)> = Vec::new();
    for (_, idx) in scored {
        let (start, end) = if transcript[idx].0 == "assistant" {
            (idx.saturating_sub(1), idx)
        } else {
            (idx, std::cmp::min(idx + 1, transcript.len() - 1))
        };
        if let Some(last) = windows.last_mut() {
            if start <= last.1 + 1 {
                last.1 = std::cmp::max(last.1, end);
                continue;
            }
        }
        windows.push((start, end));
    }

    windows
}

fn score_entry_relevance(instruction: &str, entry: &MemoryIndexEntry) -> usize {
    let mut haystack = entry.topic.clone();
    haystack.push(' ');
    haystack.push_str(&entry.file);
    haystack.push(' ');
    haystack.push_str(&entry.tags.join(" "));
    haystack.push(' ');
    haystack.push_str(&entry.note);
    score_text_relevance(instruction, &haystack)
        + usize::from(is_procedure_entry(entry))
        + usize::from(
            entry
                .tags
                .iter()
                .any(|tag| matches!(tag.as_str(), "procedure" | "workflow" | "playbook")),
        )
}

fn score_text_relevance(instruction: &str, haystack: &str) -> usize {
    let instruction_tokens = tokenize(instruction);
    if instruction_tokens.is_empty() {
        return 0;
    }

    let mut score = tokenize(haystack).intersection(&instruction_tokens).count();
    let lower_instruction = instruction.to_ascii_lowercase();
    let lower_haystack = haystack.to_ascii_lowercase();
    if lower_haystack.contains(&lower_instruction) || lower_instruction.contains(&lower_haystack) {
        score += 2;
    }
    score
}

fn tokenize(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            maybe_insert_token(&mut tokens, &current);
            current.clear();
        }
    }

    if !current.is_empty() {
        maybe_insert_token(&mut tokens, &current);
    }

    tokens
}

fn maybe_insert_token(tokens: &mut HashSet<String>, token: &str) {
    if token.len() < 3 || STOP_WORDS.contains(&token) {
        return;
    }
    tokens.insert(token.to_string());
}

fn looks_like_recall_query(lower_instruction: &str) -> bool {
    [
        "remember",
        "earlier",
        "previous",
        "before",
        "last time",
        "you said",
        "i said",
        "we talked",
        "did we",
        "what did",
        "history",
        "transcript",
        "conversation",
        "recall",
        "restart",
    ]
    .iter()
    .any(|needle| lower_instruction.contains(needle))
}

fn normalize_memory_file(file: &str) -> String {
    file.trim()
        .trim_start_matches("./")
        .trim_start_matches(".topagent/")
        .to_string()
}

fn display_memory_file(file: &str) -> String {
    let normalized = normalize_memory_file(file);
    if normalized.starts_with("topics/")
        || normalized.starts_with("lessons/")
        || normalized.starts_with("plans/")
        || normalized.starts_with("procedures/")
    {
        normalized
    } else {
        format!("topics/{normalized}")
    }
}

fn is_procedure_entry(entry: &MemoryIndexEntry) -> bool {
    normalize_memory_file(&entry.file).starts_with("procedures/")
}

fn allowed_memory_prefix(contract: &BehaviorContract, normalized: &str) -> bool {
    let topic_prefix = format!("{}/", contract.memory.topic_file_relative_dir);
    if normalized.starts_with(&topic_prefix) {
        return true;
    }

    contract
        .memory
        .archival_relative_dirs
        .iter()
        .map(|dir| format!("{dir}/"))
        .any(|prefix| normalized.starts_with(&prefix))
}

fn limit_text_block(text: &str, max_bytes: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let mut limited = trimmed[..end].trim_end().to_string();
    limited.push_str("\n[Topic excerpt truncated]");
    limited
}

fn compact_text_line(text: &str, max_bytes: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= max_bytes {
        return collapsed;
    }

    let mut end = max_bytes;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    let mut limited = collapsed[..end].trim_end().to_string();
    limited.push_str("...");
    limited
}

fn compact_note(parts: &[Option<String>], max_chars: usize) -> String {
    let mut compact = String::new();
    for part in parts.iter().flatten() {
        if part.trim().is_empty() {
            continue;
        }
        if !compact.is_empty() {
            compact.push_str("; ");
        }
        compact.push_str(part.trim());
    }
    compact_text_line(&compact, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use topagent_core::{
        ExecutionContext, RuntimeOptions, SecretRegistry, TaskMode, TaskResult, ToolTraceStep,
        VerificationCommand,
    };

    fn write_memory_index(workspace: &Path, body: &str) {
        let path = workspace.join(MEMORY_INDEX_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_topic(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_TOPICS_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_lesson(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_LESSONS_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_plan(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_PLANS_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_procedure(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn verified_task_result() -> TaskResult {
        TaskResult::new("Unified the model control path and reran the CLI test suite.".to_string())
            .with_files_changed(vec![
                "crates/topagent-cli/src/config.rs".to_string(),
                "crates/topagent-cli/src/service.rs".to_string(),
            ])
            .with_verification_command(VerificationCommand {
                command: "cargo test -p topagent-cli".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            })
    }

    fn strong_verified_task_result(output: &str) -> TaskResult {
        TaskResult::new(
            "Hardened the approval mailbox compaction flow and reran the CLI test suite."
                .to_string(),
        )
        .with_files_changed(vec![
            "crates/topagent-core/src/approval.rs".to_string(),
            "crates/topagent-core/src/run_state.rs".to_string(),
        ])
        .with_tool_trace(vec![
            ToolTraceStep {
                tool_name: "read".to_string(),
                summary: "read crates/topagent-core/src/approval.rs".to_string(),
            },
            ToolTraceStep {
                tool_name: "edit".to_string(),
                summary: "edit crates/topagent-core/src/approval.rs".to_string(),
            },
            ToolTraceStep {
                tool_name: "bash".to_string(),
                summary: "verification: cargo test -p topagent-cli".to_string(),
            },
        ])
        .with_verification_command(VerificationCommand {
            command: "cargo test -p topagent-cli".to_string(),
            output: format!("first pass failed: {output}"),
            exit_code: 1,
            succeeded: false,
        })
        .with_verification_command(VerificationCommand {
            command: "cargo test -p topagent-cli".to_string(),
            output: format!("final pass ok: {output}"),
            exit_code: 0,
            succeeded: true,
        })
    }

    fn strong_plan() -> Plan {
        let mut plan = Plan::new();
        plan.add_item("Inspect the approval mailbox and compaction flow".to_string());
        plan.add_item("Preserve pending approval anchors through the state transition".to_string());
        plan.add_item("Rerun the CLI verification and confirm the proof stays honest".to_string());
        plan
    }

    #[test]
    fn test_ensure_layout_creates_index_and_topics_dir() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();

        assert!(temp.path().join(MEMORY_INDEX_RELATIVE_PATH).is_file());
        assert!(temp.path().join(MEMORY_TOPICS_RELATIVE_DIR).is_dir());
    }

    #[test]
    fn test_consolidate_deduplicates_exact_entries() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: keep this\n- topic: architecture | file: topics/architecture.md | status: verified | note: keep this\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.duplicates_removed, 1);
        assert_eq!(rewritten.matches("topic: architecture").count(), 1);
    }

    #[test]
    fn test_always_loaded_index_stays_small() {
        let temp = TempDir::new().unwrap();
        let mut body = String::from("# TopAgent Memory Index\n\n");
        for idx in 0..40 {
            body.push_str(&format!(
                "- topic: topic-{idx} | file: topics/topic-{idx}.md | status: verified | note: durable note {idx} with enough text to make the line non-trivial\n"
            ));
        }
        write_memory_index(temp.path(), &body);

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt("review the workspace memory posture", None)
            .unwrap();
        let rendered = prompt.prompt.clone().unwrap();

        assert!(
            prompt.stats.index_prompt_bytes <= memory_contract().memory.max_index_prompt_bytes + 80
        );
        assert!(rendered.contains("Always-Loaded Index"));
        assert!(rendered.contains("omitted to keep startup memory cheap"));
    }

    #[test]
    fn test_topic_files_are_lazy_loaded_by_relevance() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | tags: runtime, session | note: agent lifecycle and session model\n- topic: security | file: topics/security.md | status: verified | tags: secret, redaction, telegram | note: do not persist secrets or redacted content\n",
        );
        write_topic(
            temp.path(),
            "architecture.md",
            "# Architecture\nsession details",
        );
        write_topic(
            temp.path(),
            "security.md",
            "# Security\nsecret handling details",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt("audit telegram secret redaction behavior", None)
            .unwrap();
        let rendered = prompt.prompt.clone().unwrap();

        assert_eq!(prompt.stats.loaded_items, vec!["security".to_string()]);
        assert!(rendered.contains("# Security"));
        assert!(!rendered.contains("# Architecture"));
    }

    #[test]
    fn test_transcript_search_returns_targeted_snippets_only() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let messages = vec![
            Message::user("remember the canary phrase"),
            Message::assistant("stored the canary phrase"),
            Message::user("also note the oak branch"),
            Message::assistant("stored the oak branch"),
            Message::user("unrelated chatter"),
            Message::assistant("more unrelated chatter"),
        ];

        let prompt = memory
            .build_prompt(
                "what was the canary phrase I mentioned earlier?",
                Some(&messages),
            )
            .unwrap();
        let rendered = prompt.prompt.clone().unwrap();

        assert_eq!(prompt.stats.transcript_snippets, 1);
        assert!(rendered.contains("canary phrase"));
        assert!(!rendered.contains("oak branch"));
        assert!(!rendered.contains("unrelated chatter"));
    }

    #[test]
    fn test_recall_query_without_keyword_match_falls_back_to_recent_exchange() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let messages = vec![
            Message::user("first exchange"),
            Message::assistant("first reply"),
            Message::user("second exchange"),
            Message::assistant("second reply"),
            Message::user("third exchange"),
            Message::assistant("third reply"),
        ];

        let prompt = memory
            .build_prompt(
                "what did we talk about before the restart?",
                Some(&messages),
            )
            .unwrap();
        let rendered = prompt.prompt.clone().unwrap();

        assert!(rendered.contains("second exchange"));
        assert!(rendered.contains("third reply"));
        assert!(!rendered.contains("first exchange"));
    }

    #[test]
    fn test_prompt_marks_memory_as_skeptical() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: tentative | note: old assumption\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory.build_prompt("inspect architecture", None).unwrap();
        let rendered = prompt.prompt.clone().unwrap();

        assert!(rendered.contains("Treat every memory item below as a hint, not truth"));
        assert!(rendered.contains("current state wins"));
    }

    #[test]
    fn test_promote_verified_task_creates_lesson_procedure_and_trajectory() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair the approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("super-secret-output-value"),
            &strong_plan(),
            false,
        )
        .unwrap();

        assert!(report.lesson_file.is_some());
        assert!(report.procedure_file.is_some());
        assert!(report.trajectory_file.is_some());

        let lesson_path = temp.path().join(report.lesson_file.unwrap());
        let procedure_path = temp.path().join(report.procedure_file.unwrap());
        let trajectory_path = temp.path().join(report.trajectory_file.unwrap());
        let memory_index =
            fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();
        let lesson = fs::read_to_string(&lesson_path).unwrap();
        let procedure = fs::read_to_string(&procedure_path).unwrap();
        let trajectory = fs::read_to_string(&trajectory_path).unwrap();

        assert!(lesson_path.is_file());
        assert!(procedure_path.is_file());
        assert!(trajectory_path.is_file());
        assert!(memory_index.contains("file: lessons/"));
        assert!(memory_index.contains("file: procedures/"));
        assert!(lesson.starts_with("# "));
        assert!(procedure.contains("## Steps"));
        assert!(procedure.contains("**Source Trajectory:** .topagent/trajectories/"));
        assert_ne!(lesson.lines().next(), procedure.lines().next());
        assert!(trajectory.contains("\"tool_sequence\""));
        assert!(trajectory.contains("\"verification\""));
        assert!(trajectory.contains("\"stored_outputs\": false"));
        assert!(!trajectory.contains("super-secret-output-value"));
    }

    #[test]
    fn test_promote_verified_task_skips_without_passing_verification() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let mut failed = verified_task_result();
        failed = failed.with_verification_command(VerificationCommand {
            command: "cargo test -p topagent-cli".to_string(),
            output: "fail".to_string(),
            exit_code: 1,
            succeeded: false,
        });

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Unify the model control path and rerun CLI tests",
            TaskMode::PlanAndExecute,
            &failed,
            &Plan::new(),
            false,
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_skips_trivial_verified_work() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let trivial = TaskResult::new("Updated one file and reran one verification.".to_string())
            .with_files_changed(vec!["README.md".to_string()])
            .with_verification_command(VerificationCommand {
                command: "cargo test -p topagent-cli".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            });
        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Update one README line and rerun the CLI test",
            TaskMode::PlanAndExecute,
            &trivial,
            &Plan::new(),
            false,
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_skips_when_memory_was_already_written() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair the approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("already saved elsewhere"),
            &strong_plan(),
            true,
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_supersedes_matching_procedure() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let first = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("first output"),
            &strong_plan(),
            false,
        )
        .unwrap();
        let second = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow with pending anchor retention",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("second output"),
            &strong_plan(),
            false,
        )
        .unwrap();

        let first_procedure = first.procedure_file.unwrap();
        let second_procedure = second.procedure_file.unwrap();
        assert_eq!(
            second.superseded_procedure_file.as_deref(),
            Some(first_procedure.as_str())
        );

        let old = parse_saved_procedure(&temp.path().join(&first_procedure))
            .unwrap()
            .unwrap();
        let new = parse_saved_procedure(&temp.path().join(&second_procedure))
            .unwrap()
            .unwrap();
        assert_eq!(old.status, ProcedureStatus::Superseded);
        assert_eq!(new.status, ProcedureStatus::Active);

        let prompt = memory
            .build_prompt("repair approval mailbox compaction workflow", None)
            .unwrap();
        assert!(prompt.stats.loaded_items.contains(&new.title));
        assert!(!prompt.stats.loaded_items.contains(&old.title));
    }

    #[test]
    fn test_build_prompt_loads_only_small_relevant_procedure_subset() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();

        let procedures = [
            ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Inspect the mailbox.".to_string(),
                    "Compact safely.".to_string(),
                ],
                pitfalls: vec!["Do not drop pending approvals.".to_string()],
                verification: "cargo test -p topagent-core approval".to_string(),
                source_task: Some("approval mailbox compaction".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
            ProcedureDraft {
                title: "Approval mailbox restore flow".to_string(),
                when_to_use: "Use for restoring approval mailbox state.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Restore the checkpoint.".to_string(),
                    "Rebuild anchors.".to_string(),
                ],
                pitfalls: vec!["Do not keep stale transcript state.".to_string()],
                verification: "cargo test -p topagent-cli telegram".to_string(),
                source_task: Some("approval mailbox restore".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
            ProcedureDraft {
                title: "Operator response tone guide".to_string(),
                when_to_use: "Use when editing operator-facing prose.".to_string(),
                prerequisites: vec!["Match repo tone.".to_string()],
                steps: vec!["Keep answers concise.".to_string()],
                pitfalls: vec!["Do not add fluff.".to_string()],
                verification: "cargo test -p topagent-cli".to_string(),
                source_task: Some("operator response tone".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
        ];

        for procedure in procedures {
            save_procedure(&memory.procedures_dir, &procedure).unwrap();
        }
        memory.consolidate_memory_if_needed().unwrap();

        let prompt = memory
            .build_prompt("repair approval mailbox compaction and restore flow", None)
            .unwrap();
        assert_eq!(prompt.stats.loaded_items.len(), 2);
        assert!(prompt
            .stats
            .loaded_items
            .contains(&"Approval mailbox compaction playbook".to_string()));
        assert!(prompt
            .stats
            .loaded_items
            .contains(&"Approval mailbox restore flow".to_string()));
        assert!(!prompt
            .stats
            .loaded_items
            .contains(&"Operator response tone guide".to_string()));
    }

    #[test]
    fn test_build_prompt_skips_superseded_procedure_entries() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_procedure(
            temp.path(),
            "100-approval-old.md",
            "# Old Approval Mailbox Procedure\n\n**Saved:** <t:100>\n**Status:** superseded\n**When To Use:** Use for old approval mailbox compaction work.\n**Verification:** cargo test -p topagent-core approval\n**Superseded By:** .topagent/procedures/200-approval-new.md\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Inspect the old flow.\n\n## Pitfalls\n\n- Do not use this anymore.\n",
        );
        write_procedure(
            temp.path(),
            "200-approval-new.md",
            "# New Approval Mailbox Procedure\n\n**Saved:** <t:200>\n**Status:** active\n**When To Use:** Use for approval mailbox compaction with pending anchor retention.\n**Verification:** cargo test -p topagent-core approval\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Preserve pending anchors.\n\n## Pitfalls\n\n- Do not drop pending approvals.\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.consolidate_memory_if_needed().unwrap();

        let prompt = memory
            .build_prompt("approval mailbox compaction", None)
            .unwrap();
        let rendered = prompt.prompt.unwrap();
        assert!(rendered.contains("New Approval Mailbox Procedure"));
        assert!(!rendered.contains("Old Approval Mailbox Procedure"));
    }

    #[test]
    fn test_promote_verified_task_redacts_registered_secrets_from_saved_artifacts() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let mut secrets = SecretRegistry::new();
        secrets.register("super-secret-output-value");
        let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_secrets(secrets);
        let options = RuntimeOptions::default();

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("super-secret-output-value"),
            &strong_plan(),
            false,
        )
        .unwrap();

        let lesson = fs::read_to_string(temp.path().join(report.lesson_file.unwrap())).unwrap();
        let procedure =
            fs::read_to_string(temp.path().join(report.procedure_file.unwrap())).unwrap();
        let trajectory =
            fs::read_to_string(temp.path().join(report.trajectory_file.unwrap())).unwrap();

        assert!(!lesson.contains("super-secret-output-value"));
        assert!(!procedure.contains("super-secret-output-value"));
        assert!(!trajectory.contains("super-secret-output-value"));
        assert!(trajectory.contains("[REDACTED]") || !trajectory.contains("first pass failed"));
    }

    #[test]
    fn test_consolidate_promotes_saved_lesson_with_absolute_date() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_lesson(
            temp.path(),
            "1700000000-approval-mailbox.md",
            "# Approval Mailbox\n\n**Saved:** <t:1700000000>\n\n---\n\n## What Changed\n\nAdded mailbox persistence within a live run.\n\n## What Was Learned\n\nPending approvals must stay visible after compaction.\n\n## Reuse Next Time\n\nKeep the mailbox as the canonical approval artifact.\n\n---\n*Saved by topagent*\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_lessons, 1);
        assert!(report.normalized_dates >= 1);
        assert!(rewritten.contains("topic: Approval Mailbox"));
        assert!(rewritten.contains("file: lessons/1700000000-approval-mailbox.md"));
        assert!(rewritten.contains("saved 2023-11-14"));
    }

    #[test]
    fn test_consolidate_prefers_verified_entry_and_prunes_stale_duplicate() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: approval mailbox | file: topics/approval.md | status: verified | tags: approval | note: operator approval gates runtime mutations\n- topic: approval mailbox | file: topics/approval.md | status: stale | tags: approval | note: runtime still allows mutation without approval\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.contradictions_resolved, 1);
        assert_eq!(report.stale_entries_pruned, 1);
        assert_eq!(rewritten.matches("topic: approval mailbox").count(), 1);
        assert!(rewritten.contains("status: verified"));
        assert!(!rewritten.contains("status: stale"));
    }

    #[test]
    fn test_consolidate_merges_saved_plan_duplicates_by_title() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_plan(
            temp.path(),
            "1700000100-release-flow.md",
            "# Release Flow\n\n**Saved:** <t:1700000100>\n\n**Task:** ship the current patch safely\n\n---\n\n## Plan Items\n\n- [>] run cargo fmt --all --check\n- [ ] run cargo test --workspace\n\n---\n*Saved by topagent*\n",
        );
        write_plan(
            temp.path(),
            "1700000200-release-flow.md",
            "# Release Flow\n\n**Saved:** <t:1700000200>\n\n**Task:** ship the current patch safely\n\n---\n\n## Plan Items\n\n- [>] run cargo clippy --workspace --all-targets -- -D warnings\n- [ ] run cargo test --workspace\n\n---\n*Saved by topagent*\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_plans, 1);
        assert_eq!(report.merged_entries, 1);
        assert_eq!(rewritten.matches("topic: Release Flow").count(), 1);
        assert!(rewritten.contains("file: plans/1700000200-release-flow.md"));
        assert!(rewritten.contains("saved 2023-11-14"));
    }

    #[test]
    fn test_consolidate_prunes_curated_lessons_to_policy_limit() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");

        for idx in 0..(memory_contract().memory.max_curated_lessons + 2) {
            let timestamp = 1700001000 + idx as i64;
            write_lesson(
                temp.path(),
                &format!("{timestamp}-lesson-{idx}.md"),
                &format!(
                    "# Lesson {idx}\n\n**Saved:** <t:{timestamp}>\n\n---\n\n## What Changed\n\nUpdated item {idx}.\n\n## What Was Learned\n\nLesson {idx} remains useful for future runs.\n\n---\n*Saved by topagent*\n"
                ),
            );
        }

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(
            rewritten.matches("| file: lessons/").count(),
            memory_contract().memory.max_curated_lessons
        );
        assert_eq!(
            report.promoted_lessons,
            memory_contract().memory.max_curated_lessons
        );
        assert!(report.pruned_entries >= 2);
    }
}

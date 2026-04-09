mod memory_consolidation;

use self::memory_consolidation::{
    parse_saved_lesson, parse_saved_plan, render_saved_lesson_excerpt, render_saved_plan_excerpt,
    MemoryIndexEntry,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use topagent_core::context::ToolContext;
use topagent_core::tools::{SaveLessonArgs, SaveLessonTool, SavePlanArgs, SavePlanTool, Tool};
use topagent_core::{
    BehaviorContract, ExecutionContext, Message, Plan, Role, RuntimeOptions, TaskResult,
};
use tracing::warn;

use crate::managed_files::write_managed_file;

const MEMORY_ROOT_DIR: &str = ".topagent";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_TOPICS_RELATIVE_DIR: &str = ".topagent/topics";
pub(crate) const MEMORY_LESSONS_RELATIVE_DIR: &str = ".topagent/lessons";
pub(crate) const MEMORY_PLANS_RELATIVE_DIR: &str = ".topagent/plans";
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
}

impl WorkspaceMemory {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self {
            index_path: workspace_root.join(MEMORY_INDEX_RELATIVE_PATH),
            topics_dir: workspace_root.join(MEMORY_TOPICS_RELATIVE_DIR),
            lessons_dir: workspace_root.join(MEMORY_LESSONS_RELATIVE_DIR),
            plans_dir: workspace_root.join(MEMORY_PLANS_RELATIVE_DIR),
            workspace_root,
        }
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn ensure_layout(&self) -> Result<()> {
        std::fs::create_dir_all(&self.topics_dir)
            .with_context(|| format!("failed to create {}", self.topics_dir.display()))?;

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
        let durable_load = self.render_durable_notes_section(&contract, instruction, &entries)?;
        let transcript_section = transcript_messages
            .and_then(|messages| render_transcript_section(&contract, instruction, messages));

        if index_section.is_none() && durable_load.section.is_none() && transcript_section.is_none()
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

        if let Some(durable_section) = durable_load.section {
            stats.loaded_items = durable_load.loaded_items;
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

    fn render_durable_notes_section(
        &self,
        contract: &BehaviorContract,
        instruction: &str,
        entries: &[MemoryIndexEntry],
    ) -> Result<TopicLoad> {
        let mut scored_entries = entries
            .iter()
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
pub(crate) struct TaskDistillationReport {
    pub lesson_file: Option<String>,
    pub plan_file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TranscriptSection {
    section: String,
    snippet_count: usize,
}

pub(crate) fn distill_verified_task(
    memory: &WorkspaceMemory,
    ctx: &ExecutionContext,
    options: &RuntimeOptions,
    instruction: &str,
    task_result: &TaskResult,
    plan: &Plan,
    durable_memory_written: bool,
) -> Result<TaskDistillationReport> {
    if durable_memory_written
        || instruction.trim().is_empty()
        || !task_result.final_verification_passed()
        || !task_result.has_files_changed()
        || task_result.has_unresolved_issues()
    {
        return Ok(TaskDistillationReport::default());
    }

    memory.ensure_layout()?;

    let tool_ctx = ToolContext::new(ctx, options);
    let lesson_title = compact_text_line(&normalize_task_text(instruction), 72);
    if lesson_title.is_empty() {
        return Ok(TaskDistillationReport::default());
    }

    let changed_files = summarize_changed_files(task_result.files_changed());
    let verification_command = task_result
        .latest_verification_command()
        .map(|command| compact_text_line(&command.command, 96));
    let lesson_args = SaveLessonArgs {
        title: lesson_title.clone(),
        what_changed: build_distilled_what_changed(&task_result.outcome_summary, &changed_files),
        what_learned: build_distilled_what_learned(&changed_files, verification_command.as_deref()),
        reuse_next_time: verification_command
            .map(|command| format!("Reuse `{command}` as the completion check for similar edits.")),
        avoid_next_time: None,
    };

    let lesson_output = SaveLessonTool::new()
        .execute(
            serde_json::to_value(lesson_args).context("failed to serialize lesson args")?,
            &tool_ctx,
        )
        .map_err(anyhow::Error::new)?;

    let mut report = TaskDistillationReport {
        lesson_file: extract_saved_artifact_path(&lesson_output, "Lesson saved to "),
        plan_file: None,
    };

    if should_save_reusable_plan(plan) {
        let plan_title = compact_text_line(&format!("{lesson_title} procedure"), 80);
        let plan_tool = SavePlanTool::with_plan(Arc::new(Mutex::new(plan.clone())));
        let plan_output = plan_tool
            .execute(
                serde_json::to_value(SavePlanArgs {
                    title: plan_title,
                    task: Some(compact_text_line(&normalize_task_text(instruction), 160)),
                })
                .context("failed to serialize plan args")?,
                &tool_ctx,
            )
            .map_err(anyhow::Error::new)?;
        report.plan_file = extract_saved_artifact_path(&plan_output, "Plan saved to ");
    }

    if report.lesson_file.is_some() || report.plan_file.is_some() {
        memory.consolidate_memory_if_needed()?;
    }

    Ok(report)
}

fn normalize_task_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

fn build_distilled_what_learned(changed_files: &str, verification_command: Option<&str>) -> String {
    match verification_command {
        Some(command) => format!(
            "For similar work, finish changes in {changed_files} by rerunning `{command}` before considering the task complete."
        ),
        None => format!(
            "For similar work, keep the edits in {changed_files} tied to an explicit passing verification step."
        ),
    }
}

fn should_save_reusable_plan(plan: &Plan) -> bool {
    plan.items().len() >= 2
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
    let instruction_tokens = tokenize(instruction);
    if instruction_tokens.is_empty() {
        return 0;
    }

    let mut haystack = entry.topic.clone();
    haystack.push(' ');
    haystack.push_str(&entry.file);
    haystack.push(' ');
    haystack.push_str(&entry.tags.join(" "));
    haystack.push(' ');
    haystack.push_str(&entry.note);

    let mut score = tokenize(&haystack)
        .intersection(&instruction_tokens)
        .count();
    let lower_instruction = instruction.to_ascii_lowercase();
    if lower_instruction.contains(&entry.topic.to_ascii_lowercase()) {
        score += 2;
    }
    if entry
        .tags
        .iter()
        .any(|tag| lower_instruction.contains(tag.as_str()))
    {
        score += 1;
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
    {
        normalized
    } else {
        format!("topics/{normalized}")
    }
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
    use topagent_core::{ExecutionContext, RuntimeOptions, TaskResult, VerificationCommand};

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
    fn test_distill_verified_task_saves_lesson_and_plan() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let mut plan = Plan::new();
        plan.add_item("Inspect the model config path".to_string());
        plan.add_item("Unify the CLI and Telegram resolution flow".to_string());

        let report = distill_verified_task(
            &memory,
            &ctx,
            &options,
            "Unify the model control path and rerun CLI tests",
            &verified_task_result(),
            &plan,
            false,
        )
        .unwrap();

        assert!(report.lesson_file.is_some());
        assert!(report.plan_file.is_some());

        let lesson_path = temp.path().join(report.lesson_file.unwrap());
        let plan_path = temp.path().join(report.plan_file.unwrap());
        let memory_index =
            fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert!(lesson_path.is_file());
        assert!(plan_path.is_file());
        assert!(memory_index.contains("file: lessons/"));
        assert!(memory_index.contains("file: plans/"));
    }

    #[test]
    fn test_distill_verified_task_skips_without_passing_verification() {
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

        let report = distill_verified_task(
            &memory,
            &ctx,
            &options,
            "Unify the model control path and rerun CLI tests",
            &failed,
            &Plan::new(),
            false,
        )
        .unwrap();

        assert_eq!(report, TaskDistillationReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PLANS_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_distill_verified_task_skips_when_memory_was_already_written() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let mut plan = Plan::new();
        plan.add_item("Inspect the config path".to_string());
        plan.add_item("Rerun the CLI tests".to_string());

        let report = distill_verified_task(
            &memory,
            &ctx,
            &options,
            "Unify the model control path and rerun CLI tests",
            &verified_task_result(),
            &plan,
            true,
        )
        .unwrap();

        assert_eq!(report, TaskDistillationReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PLANS_RELATIVE_DIR).exists());
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

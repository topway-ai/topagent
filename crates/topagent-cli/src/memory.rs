use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use time::OffsetDateTime;
use topagent_core::{BehaviorContract, Message, Role};
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConsolidationReport {
    pub index_entries_before: usize,
    pub index_entries_after: usize,
    pub duplicates_removed: usize,
    pub merged_entries: usize,
    pub contradictions_resolved: usize,
    pub stale_entries_pruned: usize,
    pub promoted_lessons: usize,
    pub promoted_plans: usize,
    pub normalized_dates: usize,
    pub pruned_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryIndexEntry {
    topic: String,
    file: String,
    status: String,
    tags: Vec<String>,
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DurableMemoryCategory {
    RepoOperational,
    OperatorPreference,
    ReusableLesson,
    StaleCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemorySourceKind {
    ManualIndex,
    SavedLesson,
    SavedPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryCandidate {
    entry: MemoryIndexEntry,
    category: DurableMemoryCategory,
    source: MemorySourceKind,
    saved_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct MemoryOrientation {
    prelude_lines: Vec<String>,
    index_entries: Vec<MemoryIndexEntry>,
    lesson_files: Vec<PathBuf>,
    plan_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ParsedLesson {
    filename: String,
    title: String,
    saved_at: Option<i64>,
    what_learned: String,
    reuse_next_time: Option<String>,
    avoid_next_time: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedPlan {
    filename: String,
    title: String,
    saved_at: Option<i64>,
    task: Option<String>,
    items: Vec<String>,
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

    pub(crate) fn consolidate_memory_if_needed(&self) -> Result<ConsolidationReport> {
        self.ensure_layout()?;

        let orientation = self.orient_memory_state()?;
        let mut report = ConsolidationReport {
            index_entries_before: orientation.index_entries.len(),
            ..ConsolidationReport::default()
        };

        let gathered = self.gather_candidates(&orientation, &mut report)?;
        let consolidated = consolidate_candidates(gathered, &mut report);
        let pruned = prune_candidates(&memory_contract(), consolidated, &mut report);

        let rewritten =
            render_index_document(&memory_contract(), &orientation.prelude_lines, &pruned);
        write_managed_file(&self.index_path, &rewritten, false)?;
        report.index_entries_after = pruned.len();

        Ok(report)
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

    fn load_index_entries(&self) -> Result<Vec<MemoryIndexEntry>> {
        if !self.index_path.exists() {
            return Ok(Vec::new());
        }

        let raw = std::fs::read_to_string(&self.index_path)
            .with_context(|| format!("failed to read {}", self.index_path.display()))?;
        Ok(raw.lines().filter_map(parse_index_entry).collect())
    }

    fn orient_memory_state(&self) -> Result<MemoryOrientation> {
        let mut orientation = MemoryOrientation::default();
        if !self.index_path.exists() {
            return Ok(orientation);
        }

        let raw = std::fs::read_to_string(&self.index_path)
            .with_context(|| format!("failed to read {}", self.index_path.display()))?;
        for line in raw.lines() {
            if let Some(entry) = parse_index_entry(line) {
                orientation.index_entries.push(entry);
            } else {
                orientation.prelude_lines.push(line.to_string());
            }
        }

        orientation.lesson_files = list_markdown_files(&self.lessons_dir)?;
        orientation.plan_files = list_markdown_files(&self.plans_dir)?;
        Ok(orientation)
    }

    fn gather_candidates(
        &self,
        orientation: &MemoryOrientation,
        report: &mut ConsolidationReport,
    ) -> Result<Vec<MemoryCandidate>> {
        let mut candidates = orientation
            .index_entries
            .iter()
            .cloned()
            .map(MemoryCandidate::from_manual_entry)
            .collect::<Vec<_>>();

        let lesson_candidates = orientation
            .lesson_files
            .iter()
            .filter_map(|path| parse_saved_lesson(path).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|lesson| {
                report.normalized_dates += usize::from(lesson.saved_at.is_some());
                MemoryCandidate::from_saved_lesson(&memory_contract(), lesson)
            });
        candidates.extend(lesson_candidates);

        let plan_candidates = orientation
            .plan_files
            .iter()
            .filter_map(|path| parse_saved_plan(path).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|plan| {
                report.normalized_dates += usize::from(plan.saved_at.is_some());
                MemoryCandidate::from_saved_plan(&memory_contract(), plan)
            });
        candidates.extend(plan_candidates);

        Ok(candidates)
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
struct TranscriptSection {
    section: String,
    snippet_count: usize,
}

impl MemoryCandidate {
    fn from_manual_entry(entry: MemoryIndexEntry) -> Self {
        Self {
            category: classify_entry_category(&entry),
            entry,
            source: MemorySourceKind::ManualIndex,
            saved_at: None,
        }
    }

    fn from_saved_lesson(contract: &BehaviorContract, lesson: ParsedLesson) -> Self {
        let date = lesson.saved_at.and_then(format_saved_date);
        let tags = derived_tags(
            &[
                lesson.title.as_str(),
                lesson.what_learned.as_str(),
                lesson.reuse_next_time.as_deref().unwrap_or_default(),
                lesson.avoid_next_time.as_deref().unwrap_or_default(),
            ],
            &["lesson", "reusable", AUTO_PROMOTED_TAG],
        );
        let note = compact_note(
            &[
                date.map(|value| format!("saved {value}")),
                Some(compact_text_line(&lesson.what_learned, 80)),
                lesson
                    .reuse_next_time
                    .as_ref()
                    .map(|value| format!("reuse: {}", compact_text_line(value, 48))),
                lesson
                    .avoid_next_time
                    .as_ref()
                    .map(|value| format!("avoid: {}", compact_text_line(value, 48))),
            ],
            contract.memory.max_index_note_chars,
        );

        Self {
            entry: MemoryIndexEntry {
                topic: lesson.title,
                file: format!("lessons/{}", lesson.filename),
                status: "verified".to_string(),
                tags,
                note,
            },
            category: DurableMemoryCategory::ReusableLesson,
            source: MemorySourceKind::SavedLesson,
            saved_at: lesson.saved_at,
        }
    }

    fn from_saved_plan(contract: &BehaviorContract, plan: ParsedPlan) -> Self {
        let date = plan.saved_at.and_then(format_saved_date);
        let task = plan
            .task
            .as_deref()
            .map(|value| compact_text_line(value, 72));
        let first_item = plan.items.first().map(|value| compact_text_line(value, 48));
        let tags = derived_tags(
            &[
                plan.title.as_str(),
                plan.task.as_deref().unwrap_or_default(),
                plan.items.first().map(String::as_str).unwrap_or_default(),
            ],
            &["plan", "workflow", AUTO_PROMOTED_TAG],
        );
        let note = compact_note(
            &[
                date.map(|value| format!("saved {value}")),
                task.map(|value| format!("task: {value}")),
                first_item.map(|value| format!("starts with: {value}")),
            ],
            contract.memory.max_index_note_chars,
        );

        Self {
            entry: MemoryIndexEntry {
                topic: plan.title,
                file: format!("plans/{}", plan.filename),
                status: "tentative".to_string(),
                tags,
                note,
            },
            category: DurableMemoryCategory::RepoOperational,
            source: MemorySourceKind::SavedPlan,
            saved_at: plan.saved_at,
        }
    }
}

fn parse_index_entry(line: &str) -> Option<MemoryIndexEntry> {
    let trimmed = line.trim();
    if !trimmed.starts_with('-') {
        return None;
    }

    let mut topic = None;
    let mut file = None;
    let mut status = "tentative".to_string();
    let mut tags = Vec::new();
    let mut note = String::new();

    for part in trimmed.trim_start_matches('-').trim().split('|') {
        let (key, value) = part.split_once(':')?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        match key.as_str() {
            "topic" => topic = Some(value.to_string()),
            "file" => file = Some(normalize_memory_file(value)),
            "status" => status = normalize_status(value),
            "tags" => {
                tags = value
                    .split(',')
                    .map(|tag| tag.trim().to_ascii_lowercase())
                    .filter(|tag| !tag.is_empty())
                    .collect();
            }
            "note" => note = value.to_string(),
            _ => {}
        }
    }

    let topic = topic?;
    let file = file?;
    if topic.is_empty() || file.is_empty() {
        return None;
    }

    Some(MemoryIndexEntry {
        topic,
        file,
        status,
        tags,
        note,
    })
}

fn canonical_entry_key(entry: &MemoryIndexEntry) -> String {
    let mut tags = entry.tags.clone();
    tags.sort();
    format!(
        "{}|{}|{}|{}|{}",
        entry.topic.trim().to_ascii_lowercase(),
        normalize_memory_file(&entry.file),
        normalize_status(&entry.status),
        tags.join(","),
        entry.note.trim().to_ascii_lowercase()
    )
}

fn merge_group_key(candidate: &MemoryCandidate) -> String {
    match candidate.source {
        MemorySourceKind::SavedLesson => format!(
            "lesson|{}",
            candidate.entry.topic.trim().to_ascii_lowercase()
        ),
        MemorySourceKind::SavedPlan => {
            format!("plan|{}", candidate.entry.topic.trim().to_ascii_lowercase())
        }
        MemorySourceKind::ManualIndex => format!(
            "{}|{}",
            candidate.entry.topic.trim().to_ascii_lowercase(),
            normalize_memory_file(&candidate.entry.file)
        ),
    }
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

fn normalize_status(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "verified" => "verified".to_string(),
        "stale" => "stale".to_string(),
        _ => "tentative".to_string(),
    }
}

fn status_rank(status: &str) -> usize {
    match normalize_status(status).as_str() {
        "verified" => 3,
        "tentative" => 2,
        "stale" => 1,
        _ => 0,
    }
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

fn classify_entry_category(entry: &MemoryIndexEntry) -> DurableMemoryCategory {
    if normalize_status(&entry.status) == "stale" {
        return DurableMemoryCategory::StaleCandidate;
    }

    let lower_topic = entry.topic.to_ascii_lowercase();
    if entry.file.starts_with("lessons/")
        || entry
            .tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "lesson" | "lessons" | "reusable"))
    {
        return DurableMemoryCategory::ReusableLesson;
    }

    if lower_topic.contains("operator")
        || lower_topic.contains("preference")
        || entry
            .tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "operator" | "preference" | "style"))
    {
        return DurableMemoryCategory::OperatorPreference;
    }

    DurableMemoryCategory::RepoOperational
}

fn candidate_priority(candidate: &MemoryCandidate) -> (usize, usize, usize, i64, &str) {
    let category = match candidate.category {
        DurableMemoryCategory::OperatorPreference => 4,
        DurableMemoryCategory::RepoOperational => 3,
        DurableMemoryCategory::ReusableLesson => 2,
        DurableMemoryCategory::StaleCandidate => 1,
    };
    let source = match candidate.source {
        MemorySourceKind::ManualIndex => 3,
        MemorySourceKind::SavedLesson => 2,
        MemorySourceKind::SavedPlan => 1,
    };
    (
        category,
        status_rank(&candidate.entry.status),
        source,
        candidate.saved_at.unwrap_or_default(),
        candidate.entry.topic.as_str(),
    )
}

fn consolidate_candidates(
    candidates: Vec<MemoryCandidate>,
    report: &mut ConsolidationReport,
) -> Vec<MemoryCandidate> {
    let mut exact_seen = HashSet::new();
    let mut grouped: HashMap<String, Vec<MemoryCandidate>> = HashMap::new();

    for candidate in candidates {
        if !exact_seen.insert(canonical_entry_key(&candidate.entry)) {
            report.duplicates_removed += 1;
            continue;
        }

        let group_key = merge_group_key(&candidate);
        grouped.entry(group_key).or_default().push(candidate);
    }

    let mut merged = Vec::new();
    for mut group in grouped.into_values() {
        group.sort_by(|left, right| candidate_priority(right).cmp(&candidate_priority(left)));
        let mut winner = group.remove(0);
        if !group.is_empty() {
            report.merged_entries += group.len();
        }

        let winner_status_rank = status_rank(&winner.entry.status);
        let mut merged_tags = BTreeSet::new();
        for tag in &winner.entry.tags {
            merged_tags.insert(tag.clone());
        }

        let mut note_fragments = Vec::new();
        if !winner.entry.note.trim().is_empty() {
            note_fragments.push(winner.entry.note.trim().to_string());
        }

        for candidate in group {
            for tag in candidate.entry.tags {
                merged_tags.insert(tag);
            }

            let candidate_status_rank = status_rank(&candidate.entry.status);
            if candidate_status_rank < winner_status_rank {
                report.contradictions_resolved += 1;
                if candidate.category == DurableMemoryCategory::StaleCandidate {
                    report.stale_entries_pruned += 1;
                }
                continue;
            }

            if candidate.entry.note.trim().is_empty()
                || note_fragments
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(candidate.entry.note.trim()))
            {
                continue;
            }
            note_fragments.push(candidate.entry.note.trim().to_string());
        }

        winner.entry.tags = merged_tags.into_iter().collect();
        let max_note_chars = memory_contract().memory.max_index_note_chars;
        winner.entry.note = compact_note(
            &note_fragments
                .into_iter()
                .map(Some)
                .collect::<Vec<Option<String>>>(),
            max_note_chars,
        );
        merged.push(winner);
    }

    merged
}

fn prune_candidates(
    contract: &BehaviorContract,
    mut candidates: Vec<MemoryCandidate>,
    report: &mut ConsolidationReport,
) -> Vec<MemoryCandidate> {
    candidates.sort_by(|left, right| candidate_priority(right).cmp(&candidate_priority(left)));

    let mut kept = Vec::new();
    let mut kept_lessons = 0usize;
    let mut kept_plans = 0usize;

    for candidate in candidates {
        if candidate.category == DurableMemoryCategory::StaleCandidate {
            report.stale_entries_pruned += 1;
            report.pruned_entries += 1;
            continue;
        }

        match candidate.source {
            MemorySourceKind::SavedLesson
                if kept_lessons >= contract.memory.max_curated_lessons =>
            {
                report.pruned_entries += 1;
                continue;
            }
            MemorySourceKind::SavedPlan if kept_plans >= contract.memory.max_curated_plans => {
                report.pruned_entries += 1;
                continue;
            }
            _ => {}
        }

        match candidate.source {
            MemorySourceKind::SavedLesson => kept_lessons += 1,
            MemorySourceKind::SavedPlan => kept_plans += 1,
            MemorySourceKind::ManualIndex => {}
        }

        kept.push(candidate);
    }

    if kept.len() > contract.memory.max_index_entries {
        report.pruned_entries += kept.len() - contract.memory.max_index_entries;
        kept.truncate(contract.memory.max_index_entries);
    }

    kept.sort_by(|left, right| candidate_priority(right).cmp(&candidate_priority(left)));
    report.promoted_lessons = kept_lessons;
    report.promoted_plans = kept_plans;
    kept
}

fn render_index_document(
    contract: &BehaviorContract,
    prelude_lines: &[String],
    candidates: &[MemoryCandidate],
) -> String {
    let mut lines = if prelude_lines.is_empty() {
        contract
            .render_memory_index_template()
            .lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    } else {
        prelude_lines.to_vec()
    };

    if lines.last().is_some_and(|line| !line.trim().is_empty()) {
        lines.push(String::new());
    }

    for candidate in candidates {
        lines.push(render_index_entry(&candidate.entry));
    }

    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn render_index_entry(entry: &MemoryIndexEntry) -> String {
    let mut line = format!(
        "- topic: {} | file: {} | status: {}",
        entry.topic,
        display_memory_file(&entry.file),
        normalize_status(&entry.status)
    );
    if !entry.tags.is_empty() {
        line.push_str(" | tags: ");
        line.push_str(&entry.tags.join(", "));
    }
    if !entry.note.trim().is_empty() {
        line.push_str(" | note: ");
        line.push_str(entry.note.trim());
    }
    line
}

fn list_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn parse_saved_lesson(path: &Path) -> Result<Option<ParsedLesson>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let title = extract_heading(&raw).unwrap_or_else(|| file_stem_or_default(path, "Lesson"));
    let what_learned = extract_markdown_section(&raw, "What Was Learned").unwrap_or_default();
    if what_learned.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(ParsedLesson {
        filename: file_name_or_default(path),
        title,
        saved_at: extract_saved_timestamp(&raw),
        what_learned,
        reuse_next_time: extract_markdown_section(&raw, "Reuse Next Time"),
        avoid_next_time: extract_markdown_section(&raw, "Avoid Next Time"),
    }))
}

fn parse_saved_plan(path: &Path) -> Result<Option<ParsedPlan>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let items = extract_plan_items(&raw);
    if items.is_empty() {
        return Ok(None);
    }

    Ok(Some(ParsedPlan {
        filename: file_name_or_default(path),
        title: extract_heading(&raw).unwrap_or_else(|| file_stem_or_default(path, "Plan")),
        saved_at: extract_saved_timestamp(&raw),
        task: extract_inline_field(&raw, "**Task:**"),
        items,
    }))
}

fn extract_heading(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(|value| value.trim().to_string())
    })
}

fn extract_inline_field(contents: &str, prefix: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn extract_markdown_section(contents: &str, heading: &str) -> Option<String> {
    let start_heading = format!("## {heading}");
    let mut lines = Vec::new();
    let mut in_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == start_heading {
            in_section = true;
            continue;
        }
        if in_section && trimmed.starts_with("## ") {
            break;
        }
        if in_section {
            lines.push(line);
        }
    }

    let joined = lines.join("\n").trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

fn extract_plan_items(contents: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == "## Plan Items" {
            in_section = true;
            continue;
        }
        if in_section && trimmed.starts_with("## ") {
            break;
        }
        if !in_section {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- [ ] ") {
            items.push(item.trim().to_string());
        } else if let Some(item) = trimmed.strip_prefix("- [>] ") {
            items.push(item.trim().to_string());
        } else if let Some(item) = trimmed.strip_prefix("- [x] ") {
            items.push(item.trim().to_string());
        }
    }

    items
}

fn extract_saved_timestamp(contents: &str) -> Option<i64> {
    contents.lines().find_map(|line| {
        let start = line.find("<t:")?;
        let rest = &line[start + 3..];
        let end = rest.find('>')?;
        rest[..end].parse::<i64>().ok()
    })
}

fn format_saved_date(timestamp: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    let date = dt.date();
    Some(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    ))
}

fn render_saved_lesson_excerpt(contract: &BehaviorContract, lesson: &ParsedLesson) -> String {
    let mut excerpt = format!("# {}\n", lesson.title);
    if let Some(saved_at) = lesson.saved_at.and_then(format_saved_date) {
        excerpt.push_str(&format!("Saved: {saved_at}\n"));
    }
    excerpt.push_str(&format!(
        "What was learned: {}\n",
        compact_text_line(&lesson.what_learned, 240)
    ));
    if let Some(reuse) = &lesson.reuse_next_time {
        excerpt.push_str(&format!(
            "Reuse next time: {}\n",
            compact_text_line(reuse, 200)
        ));
    }
    if let Some(avoid) = &lesson.avoid_next_time {
        excerpt.push_str(&format!(
            "Avoid next time: {}\n",
            compact_text_line(avoid, 200)
        ));
    }
    limit_text_block(&excerpt, contract.memory.max_durable_file_prompt_bytes)
}

fn render_saved_plan_excerpt(contract: &BehaviorContract, plan: &ParsedPlan) -> String {
    let mut excerpt = format!("# {}\n", plan.title);
    if let Some(saved_at) = plan.saved_at.and_then(format_saved_date) {
        excerpt.push_str(&format!("Saved: {saved_at}\n"));
    }
    if let Some(task) = &plan.task {
        excerpt.push_str(&format!("Task: {}\n", compact_text_line(task, 220)));
    }
    excerpt.push_str("Plan items:\n");
    for item in plan.items.iter().take(5) {
        excerpt.push_str(&format!("- {}\n", compact_text_line(item, 120)));
    }
    limit_text_block(&excerpt, contract.memory.max_durable_file_prompt_bytes)
}

fn derived_tags(texts: &[&str], fixed: &[&str]) -> Vec<String> {
    let mut frequencies = HashMap::new();
    for text in texts {
        for token in tokenize(text) {
            *frequencies.entry(token).or_insert(0usize) += 1;
        }
    }

    let mut derived = frequencies.into_iter().collect::<Vec<_>>();
    derived.sort_by(|(left_token, left_count), (right_token, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_token.cmp(right_token))
    });

    let mut tags = BTreeSet::new();
    for tag in fixed {
        tags.insert((*tag).to_string());
    }
    for (token, _) in derived.into_iter().take(4) {
        tags.insert(token);
    }
    tags.into_iter().collect()
}

fn file_name_or_default(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown.md")
        .to_string()
}

fn file_stem_or_default(path: &Path, default: &str) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(default)
        .replace('-', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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

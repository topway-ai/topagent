use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use topagent_core::{BehaviorContract, Message, Role};
use tracing::warn;

use crate::managed_files::write_managed_file;

const MEMORY_ROOT_DIR: &str = ".topagent";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_TOPICS_RELATIVE_DIR: &str = ".topagent/topics";

const MAX_INDEX_PROMPT_BYTES: usize = 1_400;
const MAX_TOPIC_PROMPT_BYTES: usize = 1_200;
const MAX_TOPICS_TO_LOAD: usize = 2;
const MAX_TRANSCRIPT_PROMPT_BYTES: usize = 1_500;
const MAX_TRANSCRIPT_SNIPPETS: usize = 3;
const MAX_TRANSCRIPT_MESSAGE_BYTES: usize = 220;

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
    pub loaded_topics: Vec<String>,
    pub transcript_snippets: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConsolidationReport {
    pub duplicates_removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryIndexEntry {
    topic: String,
    file: String,
    status: String,
    tags: Vec<String>,
    note: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMemory {
    workspace_root: PathBuf,
    index_path: PathBuf,
    topics_dir: PathBuf,
}

impl WorkspaceMemory {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self {
            index_path: workspace_root.join(MEMORY_INDEX_RELATIVE_PATH),
            topics_dir: workspace_root.join(MEMORY_TOPICS_RELATIVE_DIR),
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

    pub(crate) fn consolidate_index_if_needed(&self) -> Result<ConsolidationReport> {
        if !self.index_path.exists() {
            return Ok(ConsolidationReport::default());
        }

        let raw = std::fs::read_to_string(&self.index_path)
            .with_context(|| format!("failed to read {}", self.index_path.display()))?;
        let mut seen = HashSet::new();
        let mut kept = Vec::new();
        let mut duplicates_removed = 0usize;

        for line in raw.lines() {
            match parse_index_entry(line) {
                Some(entry) => {
                    let key = canonical_entry_key(&entry);
                    if seen.insert(key) {
                        kept.push(line.to_string());
                    } else {
                        duplicates_removed += 1;
                    }
                }
                None => kept.push(line.to_string()),
            }
        }

        if duplicates_removed > 0 {
            let mut rewritten = kept.join("\n");
            if raw.ends_with('\n') {
                rewritten.push('\n');
            }
            write_managed_file(&self.index_path, &rewritten, false)?;
        }

        Ok(ConsolidationReport { duplicates_removed })
    }

    pub(crate) fn build_prompt(
        &self,
        instruction: &str,
        transcript_messages: Option<&[Message]>,
    ) -> Result<MemoryPrompt> {
        let entries = self.load_index_entries()?;
        let index_section = render_index_section(&entries);
        let topic_load = self.render_topics_section(instruction, &entries)?;
        let transcript_section = transcript_messages
            .and_then(|messages| render_transcript_section(instruction, messages));

        if index_section.is_none() && topic_load.section.is_none() && transcript_section.is_none() {
            return Ok(MemoryPrompt::default());
        }

        let mut prompt = String::new();
        prompt.push_str(&memory_contract().render_memory_prompt_preamble());

        let mut stats = MemoryPromptStats::default();

        if let Some(index_section) = index_section {
            stats.index_prompt_bytes = index_section.len();
            prompt.push_str("\n### Always-Loaded Index\n");
            prompt.push_str(&index_section);
        }

        if let Some(topic_section) = topic_load.section {
            stats.loaded_topics = topic_load.loaded_topics;
            prompt.push_str("\n### Lazy Topic Notes\n");
            prompt.push_str(&topic_section);
        }

        if let Some(transcript_section) = transcript_section {
            stats.transcript_snippets = transcript_section.snippet_count;
            prompt.push_str("\n### Transcript Evidence\n");
            prompt.push_str(
                "Relevant snippets from prior Telegram chat. Verify before relying on them.\n",
            );
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

    fn render_topics_section(
        &self,
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
        let mut loaded_topics = Vec::new();

        for (_, entry) in scored_entries.into_iter().take(MAX_TOPICS_TO_LOAD) {
            let Some(path) = self.resolve_topic_path(&entry.file) else {
                warn!(
                    "ignoring unsafe memory topic path `{}` from {}",
                    entry.file,
                    self.index_path.display()
                );
                continue;
            };

            if !path.exists() {
                continue;
            }

            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let excerpt = limit_text_block(&raw, MAX_TOPIC_PROMPT_BYTES);
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
                display_topic_file(&entry.file),
                excerpt
            ));
            loaded_topics.push(entry.topic.clone());
        }

        Ok(TopicLoad {
            section: (!section.is_empty()).then_some(section),
            loaded_topics,
        })
    }

    fn resolve_topic_path(&self, file: &str) -> Option<PathBuf> {
        let normalized = normalize_topic_file(file);
        let relative = if normalized.starts_with("topics/") {
            normalized
        } else {
            format!("topics/{normalized}")
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TopicLoad {
    section: Option<String>,
    loaded_topics: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TranscriptSection {
    section: String,
    snippet_count: usize,
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
            "file" => file = Some(normalize_topic_file(value)),
            "status" => status = value.to_ascii_lowercase(),
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
    format!(
        "{}|{}|{}|{}|{}",
        entry.topic.trim().to_ascii_lowercase(),
        entry.file.trim().to_ascii_lowercase(),
        entry.status.trim().to_ascii_lowercase(),
        entry.tags.join(","),
        entry.note.trim().to_ascii_lowercase()
    )
}

fn render_index_section(entries: &[MemoryIndexEntry]) -> Option<String> {
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
            display_topic_file(&entry.file)
        );
        if !entry.note.is_empty() {
            line.push_str(" :: ");
            line.push_str(&entry.note);
        }
        line.push('\n');

        if section.len() + line.len() > MAX_INDEX_PROMPT_BYTES {
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

fn render_transcript_section(instruction: &str, messages: &[Message]) -> Option<TranscriptSection> {
    let transcript = messages
        .iter()
        .filter_map(|message| {
            let text = message.as_text()?;
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                _ => return None,
            };
            let compact = compact_text_line(text, MAX_TRANSCRIPT_MESSAGE_BYTES);
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

    for (start, end) in windows.into_iter().take(MAX_TRANSCRIPT_SNIPPETS) {
        let mut snippet = format!("Snippet {}:\n", snippet_count + 1);
        for (role, text) in transcript.iter().skip(start).take(end - start + 1) {
            snippet.push_str(&format!("{role}: {text}\n"));
        }

        if section.len() + snippet.len() > MAX_TRANSCRIPT_PROMPT_BYTES {
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

fn normalize_topic_file(file: &str) -> String {
    file.trim()
        .trim_start_matches("./")
        .trim_start_matches(".topagent/")
        .to_string()
}

fn display_topic_file(file: &str) -> String {
    let normalized = normalize_topic_file(file);
    if normalized.starts_with("topics/") {
        normalized
    } else {
        format!("topics/{normalized}")
    }
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
        let report = memory.consolidate_index_if_needed().unwrap();
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

        assert!(prompt.stats.index_prompt_bytes <= MAX_INDEX_PROMPT_BYTES + 80);
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

        assert_eq!(prompt.stats.loaded_topics, vec!["security".to_string()]);
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
}

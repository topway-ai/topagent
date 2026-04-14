use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use topagent_core::{
    load_operator_profile, BehaviorContract, InfluenceMode, Message, PreferenceCategory, Role,
    RunTrustContext, SourceKind, SourceLabel,
};
use tracing::warn;

use super::memory_consolidation::{
    parse_saved_lesson, parse_saved_plan, render_saved_lesson_excerpt, render_saved_plan_excerpt,
    MemoryIndexEntry, MemoryIndexEntryKind,
};
use super::observation;
use super::procedures::{parse_saved_procedure, render_saved_procedure_excerpt, ProcedureStatus};
use super::{
    allowed_memory_prefix, compact_text_line, display_memory_file, limit_text_block,
    looks_like_recall_query, memory_contract, normalize_memory_file, score_text_relevance,
    tokenize, MemoryPrompt, MemoryPromptStats, WorkspaceMemory, MEMORY_ROOT_DIR,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TopicLoad {
    section: Option<String>,
    loaded_items: Vec<String>,
    loaded_files: Vec<String>,
    provenance_notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TranscriptSection {
    section: String,
    snippet_count: usize,
    provenance_notes: Vec<String>,
}

pub(super) fn build_prompt(
    memory: &WorkspaceMemory,
    instruction: &str,
    transcript_messages: Option<&[Message]>,
) -> Result<MemoryPrompt> {
    let contract = memory_contract();
    let entries = memory.load_index_entries()?;

    // Observation-aided retrieval: boost artifact scores using prior observation links
    let retrieval = observation::progressive_retrieve(memory.observations_dir(), instruction, 8, 4)
        .unwrap_or_default();
    let boosted_paths = build_boost_set(&retrieval);

    let operator_load = render_operator_section(memory, &contract, instruction)?;
    let index_section = render_index_section(&contract, &entries);
    let procedure_load =
        render_procedure_section(memory, &contract, instruction, &entries, &boosted_paths)?;
    let durable_load =
        render_durable_notes_section(memory, &contract, instruction, &entries, &boosted_paths)?;
    let transcript_section = transcript_messages
        .and_then(|messages| render_transcript_section(&contract, instruction, messages));

    if operator_load.section.is_none()
        && index_section.is_none()
        && procedure_load.section.is_none()
        && durable_load.section.is_none()
        && transcript_section.is_none()
    {
        return Ok(MemoryPrompt::default());
    }

    let mut prompt = String::new();
    prompt.push_str(&contract.render_memory_prompt_preamble());

    let mut stats = MemoryPromptStats::default();
    let mut trust_context = RunTrustContext::default();
    let operator_prompt;

    if let Some(operator_section) = operator_load.section {
        stats
            .loaded_operator_items
            .extend(operator_load.loaded_items);
        operator_prompt = Some(operator_section);
        trust_context.add_source(SourceLabel::advisory(
            SourceKind::GeneratedMemoryArtifact,
            InfluenceMode::MayDriveAction,
            "operator model from USER.md",
        ));
    } else {
        operator_prompt = None;
    }

    if let Some(index_section) = index_section {
        stats.index_prompt_bytes = index_section.len();
        prompt.push_str("\n### Always-Loaded Index\n");
        prompt.push_str(&index_section);
        trust_context.add_source(SourceLabel::advisory(
            SourceKind::GeneratedMemoryArtifact,
            InfluenceMode::DataOnly,
            "workspace memory index",
        ));
    }

    if let Some(procedure_section) = procedure_load.section {
        stats.loaded_items.extend(procedure_load.loaded_items);
        stats
            .loaded_procedure_files
            .extend(procedure_load.loaded_files);
        stats
            .provenance_notes
            .extend(procedure_load.provenance_notes);
        prompt.push_str("\n### Relevant Procedures\n");
        prompt.push_str(&procedure_section);
        trust_context.add_source(SourceLabel::advisory(
            SourceKind::GeneratedMemoryArtifact,
            InfluenceMode::MayDriveAction,
            "curated procedures",
        ));
    }

    if let Some(durable_section) = durable_load.section {
        stats.loaded_items.extend(durable_load.loaded_items);
        stats.provenance_notes.extend(durable_load.provenance_notes);
        prompt.push_str("\n### Curated Durable Notes\n");
        prompt.push_str(&durable_section);
        trust_context.add_source(SourceLabel::advisory(
            SourceKind::GeneratedMemoryArtifact,
            InfluenceMode::MayDriveAction,
            "curated workspace memory notes",
        ));
    }

    if let Some(transcript_section) = transcript_section {
        stats.transcript_snippets = transcript_section.snippet_count;
        stats.transcript_prompt_bytes = transcript_section.section.len();
        stats
            .provenance_notes
            .extend(transcript_section.provenance_notes);
        prompt.push_str("\n### Transcript Evidence\n");
        prompt.push_str(&contract.render_memory_transcript_preamble());
        prompt.push_str(&transcript_section.section);
        trust_context.add_source(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            format!(
                "{} prior transcript snippet(s)",
                transcript_section.snippet_count
            ),
        ));
    }

    let prompt = prompt.trim_end().to_string();
    stats.total_prompt_bytes = prompt.len();
    stats.observation_hints_used = retrieval.candidates.len();
    stats.provenance_notes.extend(retrieval.provenance_notes);
    stats.provenance_notes.truncate(8);

    Ok(MemoryPrompt {
        prompt: Some(prompt),
        operator_prompt,
        stats,
        trust_context,
    })
}

fn render_operator_section(
    memory: &WorkspaceMemory,
    contract: &BehaviorContract,
    instruction: &str,
) -> Result<TopicLoad> {
    let profile = load_operator_profile(&memory.workspace_root)
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    if profile.preferences.is_empty() {
        return Ok(TopicLoad::default());
    }

    let mut scored = profile
        .preferences
        .iter()
        .filter_map(|record| {
            let score = match record.category {
                PreferenceCategory::ResponseStyle => usize::MAX / 4,
                _ => {
                    let mut haystack = format!("{} {}", record.key.replace('_', " "), record.value);
                    if let Some(rationale) = &record.rationale {
                        haystack.push(' ');
                        haystack.push_str(rationale);
                    }
                    score_text_relevance(instruction, &haystack)
                }
            };
            (score > 0).then_some((score, record))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.key.cmp(&right.key))
    });

    let mut section = String::new();
    let mut loaded_items = Vec::new();

    for (_, record) in scored
        .into_iter()
        .take(contract.memory.max_operator_preferences_to_load)
    {
        let mut excerpt = format!(
            "[{}] {} :: {}",
            record.category.as_str(),
            record.key.replace('_', " "),
            compact_text_line(&record.value, 120)
        );
        if let Some(rationale) = &record.rationale {
            excerpt.push_str(&format!(" | why: {}", compact_text_line(rationale, 80)));
        }
        let excerpt = compact_text_line(&excerpt, contract.memory.max_operator_prompt_bytes);
        if !section.is_empty() {
            section.push('\n');
        }
        section.push_str(&excerpt);
        section.push('\n');
        loaded_items.push(record.key.clone());
    }

    Ok(TopicLoad {
        section: (!section.is_empty()).then_some(section),
        loaded_items,
        loaded_files: Vec::new(),
        provenance_notes: Vec::new(),
    })
}

fn render_procedure_section(
    memory: &WorkspaceMemory,
    contract: &BehaviorContract,
    instruction: &str,
    entries: &[MemoryIndexEntry],
    boosted_paths: &HashSet<String>,
) -> Result<TopicLoad> {
    let mut scored_entries = entries
        .iter()
        .filter(|entry| entry.kind() == MemoryIndexEntryKind::Procedure)
        .filter_map(|entry| {
            let mut score = score_entry_relevance(instruction, entry);
            let boosted = is_boosted(entry, boosted_paths);
            if boosted {
                score += 3;
            }
            (score > 0).then_some((score, boosted, entry))
        })
        .collect::<Vec<_>>();
    scored_entries.sort_by(
        |(left_score, _, left_entry), (right_score, _, right_entry)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_entry.topic.cmp(&right_entry.topic))
        },
    );

    let mut section = String::new();
    let mut loaded_items = Vec::new();
    let mut loaded_files = Vec::new();
    let mut provenance_notes = Vec::new();

    for (score, boosted, entry) in scored_entries
        .into_iter()
        .take(contract.memory.max_procedures_to_load)
    {
        let Some(path) = resolve_memory_path(memory, contract, &entry.file) else {
            warn!(
                "ignoring unsafe procedure path `{}` from {}",
                entry.file,
                memory.index_path.display()
            );
            continue;
        };

        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        if procedure.status != ProcedureStatus::Active {
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

        let mut note = format!(
            "procedure | {} | {} | advisory | matched: score {}",
            procedure.filename, procedure.title, score
        );
        if boosted {
            note.push_str(" +observation boost");
        }
        if procedure.reuse_count > 0 {
            note.push_str(&format!(" | reuse: {}", procedure.reuse_count));
        }
        if provenance_notes.len() < 4 {
            provenance_notes.push(compact_provenance_note(&note));
        }

        loaded_items.push(procedure.title);
        loaded_files.push(format!(".topagent/procedures/{}", procedure.filename));
    }

    Ok(TopicLoad {
        section: (!section.is_empty()).then_some(section),
        loaded_items,
        loaded_files,
        provenance_notes,
    })
}

fn render_durable_notes_section(
    memory: &WorkspaceMemory,
    contract: &BehaviorContract,
    instruction: &str,
    entries: &[MemoryIndexEntry],
    boosted_paths: &HashSet<String>,
) -> Result<TopicLoad> {
    let mut scored_entries = entries
        .iter()
        .filter(|entry| entry.kind() != MemoryIndexEntryKind::Procedure)
        .filter_map(|entry| {
            let mut score = score_entry_relevance(instruction, entry);
            let boosted = is_boosted(entry, boosted_paths);
            if boosted {
                score += 3;
            }
            (score > 0).then_some((score, boosted, entry))
        })
        .collect::<Vec<_>>();
    scored_entries.sort_by(
        |(left_score, _, left_entry), (right_score, _, right_entry)| {
            right_score
                .cmp(left_score)
                .then_with(|| left_entry.topic.cmp(&right_entry.topic))
        },
    );

    let mut section = String::new();
    let mut loaded_items = Vec::new();
    let mut provenance_notes = Vec::new();

    for (score, boosted, entry) in scored_entries
        .into_iter()
        .take(contract.memory.max_topics_to_load)
    {
        let Some(path) = resolve_memory_path(memory, contract, &entry.file) else {
            warn!(
                "ignoring unsafe memory path `{}` from {}",
                entry.file,
                memory.index_path.display()
            );
            continue;
        };

        if !path.exists() {
            continue;
        }

        let excerpt = render_memory_file_excerpt(contract, entry, &path)?;
        if excerpt.is_empty() {
            continue;
        }

        if !section.is_empty() {
            section.push('\n');
        }

        let kind_label = match entry.kind() {
            MemoryIndexEntryKind::Lesson => "lesson",
            MemoryIndexEntryKind::Plan => "plan",
            _ => "topic",
        };
        section.push_str(&format!(
            "[{}] {} ({})\n{}\n",
            entry.status,
            entry.topic,
            display_memory_file(&entry.file),
            excerpt
        ));
        loaded_items.push(entry.topic.clone());

        let mut note = format!(
            "{} | {} | {} | advisory | matched: score {}",
            kind_label, entry.file, entry.topic, score
        );
        if boosted {
            note.push_str(" +observation boost");
        }
        if provenance_notes.len() < 4 {
            provenance_notes.push(compact_provenance_note(&note));
        }
    }

    Ok(TopicLoad {
        section: (!section.is_empty()).then_some(section),
        loaded_items,
        loaded_files: Vec::new(),
        provenance_notes,
    })
}

fn resolve_memory_path(
    memory: &WorkspaceMemory,
    contract: &BehaviorContract,
    file: &str,
) -> Option<PathBuf> {
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
        memory
            .workspace_root
            .join(MEMORY_ROOT_DIR)
            .join(relative_path),
    )
}

fn render_memory_file_excerpt(
    contract: &BehaviorContract,
    entry: &MemoryIndexEntry,
    path: &Path,
) -> Result<String> {
    match entry.kind() {
        MemoryIndexEntryKind::Lesson => {
            if let Some(parsed) = parse_saved_lesson(path)? {
                return Ok(render_saved_lesson_excerpt(contract, &parsed));
            }
        }
        MemoryIndexEntryKind::Plan => {
            if let Some(parsed) = parse_saved_plan(path)? {
                return Ok(render_saved_plan_excerpt(contract, &parsed));
            }
        }
        MemoryIndexEntryKind::Procedure => {
            if let Some(parsed) = parse_saved_procedure(path)? {
                return Ok(render_saved_procedure_excerpt(contract, &parsed));
            }
        }
        MemoryIndexEntryKind::Topic => {}
    }

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(limit_text_block(
        &raw,
        contract.memory.max_durable_file_prompt_bytes,
    ))
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
        provenance_notes: vec![compact_provenance_note(&format!(
            "transcript | prior | low | {} snippet(s) | matched: {}",
            snippet_count,
            if recall_like {
                "recall-like query"
            } else {
                "keyword overlap"
            }
        ))],
    })
}

fn match_windows(
    transcript: &[(String, String)],
    instruction_tokens: &std::collections::HashSet<String>,
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

fn build_boost_set(retrieval: &observation::RetrievalResult) -> HashSet<String> {
    retrieval
        .artifact_paths
        .iter()
        .filter_map(|path| {
            // Normalize to the form used in index entries (e.g. "procedures/foo.md")
            path.strip_prefix(".topagent/").map(|s| s.to_string())
        })
        .collect()
}

fn is_boosted(entry: &MemoryIndexEntry, boosted_paths: &HashSet<String>) -> bool {
    if boosted_paths.is_empty() {
        return false;
    }
    let normalized = super::normalize_memory_file(&entry.file);
    boosted_paths.contains(&normalized)
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
        + usize::from(entry.kind() == MemoryIndexEntryKind::Procedure)
        + usize::from(
            entry
                .tags
                .iter()
                .any(|tag| matches!(tag.as_str(), "procedure" | "workflow" | "playbook")),
        )
}

const MAX_PROVENANCE_NOTE_CHARS: usize = 200;

fn compact_provenance_note(note: &str) -> String {
    if note.len() <= MAX_PROVENANCE_NOTE_CHARS {
        note.to_string()
    } else {
        format!(
            "{}...",
            &note[..MAX_PROVENANCE_NOTE_CHARS.saturating_sub(3)]
        )
    }
}

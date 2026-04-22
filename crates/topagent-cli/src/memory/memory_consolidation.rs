use super::{
    compact_note, compact_text_line, display_memory_file, limit_text_block, memory_contract,
    normalize_memory_file, procedures::parse_saved_procedure, procedures::ParsedProcedure,
    procedures::ProcedureStatus, WorkspaceMemory, AUTO_PROMOTED_TAG,
};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use topagent_core::BehaviorContract;

use crate::managed_files::write_managed_file;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConsolidationReport {
    pub index_entries_before: usize,
    pub index_entries_after: usize,
    pub duplicates_removed: usize,
    pub merged_entries: usize,
    pub contradictions_resolved: usize,
    pub stale_entries_pruned: usize,
    pub promoted_notes: usize,
    pub promoted_procedures: usize,
    pub normalized_dates: usize,
    pub pruned_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryIndexEntry {
    pub(super) title: String,
    pub(super) file: String,
    pub(super) status: String,
    pub(super) tags: Vec<String>,
    pub(super) note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemoryIndexEntryKind {
    Note,
    Procedure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DurableMemoryCategory {
    ReusableProcedure,
    DurableNote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemorySourceKind {
    ManualIndex,
    SavedNote,
    SavedProcedure,
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
    note_files: Vec<PathBuf>,
    procedure_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedNote {
    filename: String,
    title: String,
    saved_at: Option<i64>,
    what_learned: String,
    reuse_next_time: Option<String>,
    avoid_next_time: Option<String>,
}

impl WorkspaceMemory {
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

    pub(super) fn load_index_entries(&self) -> Result<Vec<MemoryIndexEntry>> {
        if !self.index_path.exists() {
            return Ok(Vec::new());
        }

        let raw = std::fs::read_to_string(&self.index_path)
            .with_context(|| format!("failed to read {}", self.index_path.display()))?;
        Ok(raw.lines().filter_map(parse_index_entry).collect())
    }

    pub(crate) fn index_entry_count(&self) -> Result<usize> {
        Ok(self.load_index_entries()?.len())
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

        orientation.note_files = list_markdown_files(&self.notes_dir)?;
        orientation.procedure_files = list_markdown_files(&self.procedures_dir)?;
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

        let note_candidates = orientation
            .note_files
            .iter()
            .filter_map(|path| parse_saved_note(path).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|note| {
                report.normalized_dates += usize::from(note.saved_at.is_some());
                MemoryCandidate::from_saved_note(&memory_contract(), note)
            });
        candidates.extend(note_candidates);

        let procedure_candidates = orientation
            .procedure_files
            .iter()
            .filter_map(|path| parse_saved_procedure(path).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|procedure| {
                report.normalized_dates += usize::from(procedure.saved_at.is_some());
                MemoryCandidate::from_saved_procedure(&memory_contract(), procedure)
            });
        candidates.extend(procedure_candidates);

        Ok(candidates)
    }
}

impl MemoryIndexEntry {
    pub(super) fn kind(&self) -> MemoryIndexEntryKind {
        let normalized = normalize_memory_file(&self.file);
        if normalized.starts_with("procedures/") {
            MemoryIndexEntryKind::Procedure
        } else {
            MemoryIndexEntryKind::Note
        }
    }
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

    fn from_saved_note(contract: &BehaviorContract, note_file: ParsedNote) -> Self {
        let date = note_file.saved_at.and_then(format_saved_date);
        let tags = derived_tags(
            &[
                note_file.title.as_str(),
                note_file.what_learned.as_str(),
                note_file.reuse_next_time.as_deref().unwrap_or_default(),
                note_file.avoid_next_time.as_deref().unwrap_or_default(),
            ],
            &["note", AUTO_PROMOTED_TAG],
        );
        let note = compact_note(
            &[
                date.map(|value| format!("saved {value}")),
                Some(compact_text_line(&note_file.what_learned, 80)),
                note_file
                    .reuse_next_time
                    .as_ref()
                    .map(|value| format!("reuse: {}", compact_text_line(value, 48))),
                note_file
                    .avoid_next_time
                    .as_ref()
                    .map(|value| format!("avoid: {}", compact_text_line(value, 48))),
            ],
            contract.memory.max_index_note_chars,
        );

        Self {
            entry: MemoryIndexEntry {
                title: note_file.title,
                file: format!("notes/{}", note_file.filename),
                status: "verified".to_string(),
                tags,
                note,
            },
            category: DurableMemoryCategory::DurableNote,
            source: MemorySourceKind::SavedNote,
            saved_at: note_file.saved_at,
        }
    }

    fn from_saved_procedure(contract: &BehaviorContract, procedure: ParsedProcedure) -> Self {
        let is_live = procedure.status == ProcedureStatus::Active;
        let date = procedure.saved_at.and_then(format_saved_date);
        let tags = derived_tags(
            &[
                procedure.title.as_str(),
                procedure.when_to_use.as_str(),
                procedure.verification.as_str(),
                procedure.source_task.as_deref().unwrap_or_default(),
            ],
            &["procedure", "workflow", "playbook", AUTO_PROMOTED_TAG],
        );
        let note = compact_note(
            &[
                date.map(|value| format!("saved {value}")),
                Some(compact_text_line(&procedure.when_to_use, 72)),
                Some(format!(
                    "verify: {}",
                    compact_text_line(&procedure.verification, 40)
                )),
                procedure
                    .source_trajectory
                    .as_ref()
                    .map(|value| format!("trajectory: {value}")),
            ],
            contract.memory.max_index_note_chars,
        );

        Self {
            entry: MemoryIndexEntry {
                title: procedure.title,
                file: format!("procedures/{}", procedure.filename),
                status: if is_live {
                    "verified".to_string()
                } else {
                    "stale".to_string()
                },
                tags,
                note,
            },
            category: if is_live {
                DurableMemoryCategory::ReusableProcedure
            } else {
                DurableMemoryCategory::DurableNote
            },
            source: MemorySourceKind::SavedProcedure,
            saved_at: procedure.saved_at,
        }
    }
}

fn parse_index_entry(line: &str) -> Option<MemoryIndexEntry> {
    let trimmed = line.trim();
    if !trimmed.starts_with('-') {
        return None;
    }

    let mut title = None;
    let mut file = None;
    let mut status = "tentative".to_string();
    let mut tags = Vec::new();
    let mut note = String::new();

    for part in trimmed.trim_start_matches('-').trim().split('|') {
        let (key, value) = part.split_once(':')?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        match key.as_str() {
            "title" => title = Some(value.to_string()),
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

    let title = title?;
    let file = file?;
    if title.is_empty() || file.is_empty() {
        return None;
    }

    Some(MemoryIndexEntry {
        title,
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
        entry.title.trim().to_ascii_lowercase(),
        normalize_memory_file(&entry.file),
        normalize_status(&entry.status),
        tags.join(","),
        entry.note.trim().to_ascii_lowercase()
    )
}

fn merge_group_key(candidate: &MemoryCandidate) -> String {
    match candidate.source {
        MemorySourceKind::SavedNote => {
            format!("note|{}", candidate.entry.title.trim().to_ascii_lowercase())
        }
        MemorySourceKind::SavedProcedure => format!(
            "procedure|{}",
            candidate.entry.title.trim().to_ascii_lowercase()
        ),
        MemorySourceKind::ManualIndex => format!(
            "{}|{}",
            candidate.entry.title.trim().to_ascii_lowercase(),
            normalize_memory_file(&candidate.entry.file)
        ),
    }
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

fn classify_entry_category(entry: &MemoryIndexEntry) -> DurableMemoryCategory {
    if normalize_status(&entry.status) == "stale" {
        return DurableMemoryCategory::DurableNote;
    }

    if entry.file.starts_with("procedures/")
        || entry
            .tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "procedure" | "workflow" | "playbook"))
    {
        return DurableMemoryCategory::ReusableProcedure;
    }

    DurableMemoryCategory::DurableNote
}

fn candidate_priority(candidate: &MemoryCandidate) -> (usize, usize, usize, i64, &str) {
    let category = match candidate.category {
        DurableMemoryCategory::ReusableProcedure => 3,
        DurableMemoryCategory::DurableNote => 2,
    };
    let source = match candidate.source {
        MemorySourceKind::ManualIndex => 3,
        MemorySourceKind::SavedNote => 2,
        MemorySourceKind::SavedProcedure => 2,
    };
    (
        category,
        status_rank(&candidate.entry.status),
        source,
        candidate.saved_at.unwrap_or_default(),
        candidate.entry.title.as_str(),
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
                if normalize_status(&candidate.entry.status) == "stale" {
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
    let mut kept_notes = 0usize;
    let mut kept_procedures = 0usize;

    for candidate in candidates {
        if normalize_status(&candidate.entry.status) == "stale" {
            report.stale_entries_pruned += 1;
            report.pruned_entries += 1;
            continue;
        }

        match candidate.source {
            MemorySourceKind::SavedNote if kept_notes >= contract.memory.max_curated_notes => {
                report.pruned_entries += 1;
                continue;
            }
            MemorySourceKind::SavedProcedure
                if kept_procedures >= contract.memory.max_curated_procedures =>
            {
                report.pruned_entries += 1;
                continue;
            }
            _ => {}
        }

        match candidate.source {
            MemorySourceKind::SavedNote => kept_notes += 1,
            MemorySourceKind::SavedProcedure => kept_procedures += 1,
            MemorySourceKind::ManualIndex => {}
        }

        kept.push(candidate);
    }

    if kept.len() > contract.memory.max_index_entries {
        report.pruned_entries += kept.len() - contract.memory.max_index_entries;
        kept.truncate(contract.memory.max_index_entries);
    }

    kept.sort_by(|left, right| candidate_priority(right).cmp(&candidate_priority(left)));
    report.promoted_notes = kept_notes;
    report.promoted_procedures = kept_procedures;
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
        "- title: {} | file: {} | status: {}",
        entry.title,
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

pub(super) fn parse_saved_note(path: &Path) -> Result<Option<ParsedNote>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let title = extract_heading(&raw).unwrap_or_else(|| file_stem_or_default(path, "Note"));
    let what_learned = extract_markdown_section(&raw, "What Was Learned").unwrap_or_default();
    if what_learned.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(ParsedNote {
        filename: file_name_or_default(path),
        title,
        saved_at: extract_saved_timestamp(&raw),
        what_learned,
        reuse_next_time: extract_markdown_section(&raw, "Reuse Next Time"),
        avoid_next_time: extract_markdown_section(&raw, "Avoid Next Time"),
    }))
}

fn extract_heading(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(|value| value.trim().to_string())
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

pub(super) fn render_saved_note_excerpt(contract: &BehaviorContract, note: &ParsedNote) -> String {
    let mut excerpt = format!("# {}\n", note.title);
    if let Some(saved_at) = note.saved_at.and_then(format_saved_date) {
        excerpt.push_str(&format!("Saved: {saved_at}\n"));
    }
    excerpt.push_str(&format!(
        "What was learned: {}\n",
        compact_text_line(&note.what_learned, 240)
    ));
    if let Some(reuse) = &note.reuse_next_time {
        excerpt.push_str(&format!(
            "Reuse next time: {}\n",
            compact_text_line(reuse, 200)
        ));
    }
    if let Some(avoid) = &note.avoid_next_time {
        excerpt.push_str(&format!(
            "Avoid next time: {}\n",
            compact_text_line(avoid, 200)
        ));
    }
    limit_text_block(&excerpt, contract.memory.max_durable_file_prompt_bytes)
}

fn derived_tags(texts: &[&str], fixed: &[&str]) -> Vec<String> {
    let mut frequencies = HashMap::new();
    for text in texts {
        for token in super::tokenize(text) {
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
    use crate::memory::{
        WorkspaceMemory, MEMORY_INDEX_RELATIVE_PATH, MEMORY_PROCEDURES_RELATIVE_DIR,
    };
    use std::fs;
    use tempfile::TempDir;

    fn write_memory_index(workspace: &Path, body: &str) {
        let path = workspace.join(MEMORY_INDEX_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_procedure(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn test_consolidate_promotes_active_procedure_into_index() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_procedure(
            temp.path(),
            "1700000300-approval-mailbox.md",
            "# Approval Mailbox Procedure\n\n**Saved:** <t:1700000300>\n**Status:** active\n**When To Use:** Use for approval mailbox compaction with pending anchor retention.\n**Verification:** cargo test -p topagent-core approval\n**Source Task:** repair approval mailbox compaction workflow\n**Source Trajectory:** .topagent/trajectories/trj-1700000300-approval-mailbox.json\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Inspect the mailbox state.\n2. Preserve pending approval anchors.\n\n## Pitfalls\n\n- Do not drop pending approvals during compaction.\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_procedures, 1);
        assert!(rewritten.contains("title: Approval Mailbox Procedure"));
        assert!(rewritten.contains("file: procedures/1700000300-approval-mailbox.md"));
        assert!(rewritten.contains("status: verified"));
        assert!(rewritten.contains("tags:"));
    }

    #[test]
    fn test_consolidate_prunes_superseded_procedure_from_index() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_procedure(
            temp.path(),
            "1700000400-approval-old.md",
            "# Approval Mailbox Procedure\n\n**Saved:** <t:1700000400>\n**Status:** superseded\n**When To Use:** Old approval mailbox compaction workflow.\n**Verification:** cargo test -p topagent-core approval\n**Superseded By:** .topagent/procedures/1700000500-approval-new.md\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Inspect the old flow.\n\n## Pitfalls\n\n- Do not keep using this procedure.\n",
        );
        write_procedure(
            temp.path(),
            "1700000500-approval-new.md",
            "# Approval Mailbox Procedure\n\n**Saved:** <t:1700000500>\n**Status:** active\n**When To Use:** Approval mailbox compaction with pending anchor retention.\n**Verification:** cargo test -p topagent-core approval\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Preserve pending approval anchors.\n\n## Pitfalls\n\n- Do not drop pending approvals.\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_procedures, 1);
        assert!(report.stale_entries_pruned >= 1);
        assert_eq!(
            rewritten
                .matches("title: Approval Mailbox Procedure")
                .count(),
            1
        );
        assert!(rewritten.contains("file: procedures/1700000500-approval-new.md"));
        assert!(!rewritten.contains("file: procedures/1700000400-approval-old.md"));
    }
}

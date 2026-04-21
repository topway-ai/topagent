use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use topagent_core::BehaviorContract;

use super::{
    artifact_filename, compact_text_line, score_text_relevance, slugify, unix_timestamp_secs,
    WorkspaceMemory,
};
use crate::managed_files::write_managed_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcedureStatus {
    Active,
    Superseded,
    Disabled,
}

impl ProcedureStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Disabled => "disabled",
        }
    }

    fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "superseded" => Self::Superseded,
            "disabled" => Self::Disabled,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedProcedure {
    pub(crate) filename: String,
    pub(crate) title: String,
    pub(crate) saved_at: Option<i64>,
    pub(crate) status: ProcedureStatus,
    pub(crate) when_to_use: String,
    pub(crate) prerequisites: Vec<String>,
    pub(crate) steps: Vec<String>,
    pub(crate) pitfalls: Vec<String>,
    pub(crate) verification: String,
    pub(crate) reuse_count: u32,
    pub(crate) revision_count: u32,
    pub(crate) last_verified_reuse_at: Option<i64>,
    pub(crate) source_task: Option<String>,
    pub(crate) source_note: Option<String>,
    pub(crate) source_trajectory: Option<String>,
    pub(crate) supersedes: Option<String>,
    pub(crate) superseded_by: Option<String>,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcedureDraft {
    pub(crate) title: String,
    pub(crate) when_to_use: String,
    pub(crate) prerequisites: Vec<String>,
    pub(crate) steps: Vec<String>,
    pub(crate) pitfalls: Vec<String>,
    pub(crate) verification: String,
    pub(crate) source_task: Option<String>,
    pub(crate) source_note: Option<String>,
    pub(crate) source_trajectory: Option<String>,
    pub(crate) supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcedureRevisionAction {
    Keep,
    Refine,
    Supersede,
}

pub(crate) fn save_procedure(
    procedures_dir: &Path,
    draft: &ProcedureDraft,
) -> Result<(String, PathBuf)> {
    std::fs::create_dir_all(procedures_dir)
        .with_context(|| format!("failed to create {}", procedures_dir.display()))?;

    let timestamp = unix_timestamp_secs();
    let filename = format!("{}-{}.md", timestamp, slugify(&draft.title, "procedure"));
    let path = procedures_dir.join(&filename);
    let content = render_procedure_markdown(&ParsedProcedure {
        filename: filename.clone(),
        title: draft.title.clone(),
        saved_at: Some(timestamp),
        status: ProcedureStatus::Active,
        when_to_use: draft.when_to_use.clone(),
        prerequisites: draft.prerequisites.clone(),
        steps: draft.steps.clone(),
        pitfalls: draft.pitfalls.clone(),
        verification: draft.verification.clone(),
        reuse_count: 0,
        revision_count: 0,
        last_verified_reuse_at: None,
        source_task: draft.source_task.clone(),
        source_note: draft.source_note.clone(),
        source_trajectory: draft.source_trajectory.clone(),
        supersedes: draft.supersedes.clone(),
        superseded_by: None,
        disabled_reason: None,
        path: path.clone(),
    });
    write_managed_file(&path, &content, false)?;
    Ok((format!(".topagent/procedures/{filename}"), path))
}

pub(crate) fn parse_saved_procedure(path: &Path) -> Result<Option<ParsedProcedure>> {
    if !path.is_file() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let title = parse_heading(&raw)?;
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    Ok(Some(ParsedProcedure {
        filename,
        title,
        saved_at: parse_saved_timestamp(&raw),
        status: ProcedureStatus::from_str(
            &parse_named_field(&raw, "**Status:**").unwrap_or_else(|| "active".to_string()),
        ),
        when_to_use: parse_named_field(&raw, "**When To Use:**").unwrap_or_default(),
        prerequisites: parse_list_section(&raw, "Prerequisites"),
        steps: parse_list_section(&raw, "Steps"),
        pitfalls: parse_list_section(&raw, "Pitfalls"),
        verification: parse_named_field(&raw, "**Verification:**").unwrap_or_default(),
        reuse_count: parse_named_field(&raw, "**Reuse Count:**")
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
        revision_count: parse_named_field(&raw, "**Revision Count:**")
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
        last_verified_reuse_at: parse_named_field(&raw, "**Last Verified Reuse:**").and_then(
            |value| {
                value
                    .trim_start_matches("<t:")
                    .trim_end_matches('>')
                    .parse()
                    .ok()
            },
        ),
        source_task: parse_named_field(&raw, "**Source Task:**"),
        source_note: parse_named_field(&raw, "**Source Note:**")
            .or_else(|| parse_named_field(&raw, "**Source Lesson:**")),
        source_trajectory: parse_named_field(&raw, "**Source Trajectory:**"),
        supersedes: parse_named_field(&raw, "**Supersedes:**"),
        superseded_by: parse_named_field(&raw, "**Superseded By:**"),
        disabled_reason: parse_named_field(&raw, "**Disabled Reason:**"),
        path: path.to_path_buf(),
    }))
}

pub(crate) fn mark_procedure_superseded(
    path: &Path,
    superseded_by: &str,
) -> Result<Option<String>> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(None);
    };
    procedure.status = ProcedureStatus::Superseded;
    procedure.superseded_by = Some(superseded_by.to_string());
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(Some(format!(".topagent/procedures/{}", procedure.filename)))
}

pub(crate) fn revise_procedure(
    path: &Path,
    draft: &ProcedureDraft,
    source_note: Option<&str>,
    source_trajectory: Option<&str>,
) -> Result<Option<String>> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(None);
    };
    procedure.status = ProcedureStatus::Active;
    procedure.disabled_reason = None;
    procedure.when_to_use = draft.when_to_use.clone();
    procedure.verification = draft.verification.clone();
    procedure.prerequisites = merge_unique_items(&procedure.prerequisites, &draft.prerequisites, 6);
    procedure.steps = merge_unique_items(&procedure.steps, &draft.steps, 8);
    procedure.pitfalls = merge_unique_items(&procedure.pitfalls, &draft.pitfalls, 6);
    procedure.reuse_count = procedure.reuse_count.saturating_add(1);
    procedure.revision_count = procedure.revision_count.saturating_add(1);
    procedure.last_verified_reuse_at = Some(unix_timestamp_secs());
    procedure.source_task = draft.source_task.clone().or(procedure.source_task);
    if let Some(source_note) = source_note {
        procedure.source_note = Some(source_note.to_string());
    }
    if let Some(source_trajectory) = source_trajectory {
        procedure.source_trajectory = Some(source_trajectory.to_string());
    }
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(Some(format!(".topagent/procedures/{}", procedure.filename)))
}

pub(crate) fn record_procedure_reuse(
    path: &Path,
    source_trajectory: Option<&str>,
) -> Result<Option<String>> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(None);
    };
    procedure.reuse_count = procedure.reuse_count.saturating_add(1);
    procedure.last_verified_reuse_at = Some(unix_timestamp_secs());
    if let Some(source_trajectory) = source_trajectory {
        procedure.source_trajectory = Some(source_trajectory.to_string());
    }
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(Some(format!(".topagent/procedures/{}", procedure.filename)))
}

pub(crate) fn set_procedure_source_trajectory(path: &Path, trajectory_file: &str) -> Result<()> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(());
    };
    procedure.source_trajectory = Some(trajectory_file.to_string());
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(())
}

pub(crate) fn disable_procedure(path: &Path, reason: Option<&str>) -> Result<Option<String>> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(None);
    };
    procedure.status = ProcedureStatus::Disabled;
    procedure.disabled_reason = reason.map(ToString::to_string);
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(Some(format!(".topagent/procedures/{}", procedure.filename)))
}

pub(crate) fn render_saved_procedure_excerpt(
    contract: &BehaviorContract,
    procedure: &ParsedProcedure,
) -> String {
    let mut lines = Vec::new();
    if !procedure.when_to_use.is_empty() {
        lines.push(format!(
            "When to use: {}",
            compact_text_line(&procedure.when_to_use, 120)
        ));
    }
    if let Some(first_step) = procedure.steps.first() {
        lines.push(format!(
            "Starts with: {}",
            compact_text_line(first_step, 96)
        ));
    }
    if !procedure.verification.is_empty() {
        lines.push(format!(
            "Verify with: {}",
            compact_text_line(&procedure.verification, 96)
        ));
    }
    if procedure.reuse_count > 0 {
        lines.push(format!("Verified reuses: {}", procedure.reuse_count));
    }
    if let Some(pitfall) = procedure.pitfalls.first() {
        lines.push(format!("Pitfall: {}", compact_text_line(pitfall, 96)));
    }

    compact_text_line(
        &lines.join(" | "),
        contract.memory.max_durable_file_prompt_bytes,
    )
}

pub(crate) fn procedure_haystack(procedure: &ParsedProcedure) -> String {
    let mut haystack = String::new();
    haystack.push_str(&procedure.title);
    haystack.push(' ');
    haystack.push_str(&procedure.when_to_use);
    haystack.push(' ');
    haystack.push_str(&procedure.verification);
    if let Some(source_task) = &procedure.source_task {
        haystack.push(' ');
        haystack.push_str(source_task);
    }
    if !procedure.steps.is_empty() {
        haystack.push(' ');
        haystack.push_str(&procedure.steps.join(" "));
    }
    if !procedure.pitfalls.is_empty() {
        haystack.push(' ');
        haystack.push_str(&procedure.pitfalls.join(" "));
    }
    haystack
}

pub(crate) fn find_matching_active_procedure(
    memory: &WorkspaceMemory,
    instruction: &str,
) -> Result<Option<ParsedProcedure>> {
    let mut best: Option<(usize, ParsedProcedure)> = None;
    for path in list_markdown_files(&memory.procedures_dir)? {
        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        if procedure.status != ProcedureStatus::Active {
            continue;
        }

        let score = procedure_match_score(instruction, &procedure);
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

pub(crate) fn find_matching_loaded_procedure(
    memory: &WorkspaceMemory,
    instruction: &str,
    loaded_procedure_files: &[String],
) -> Result<Option<ParsedProcedure>> {
    let mut best: Option<(usize, ParsedProcedure)> = None;
    for relative_path in loaded_procedure_files {
        let Some(filename) = artifact_filename(relative_path) else {
            continue;
        };
        let path = memory.procedures_dir.join(filename);
        let Some(procedure) = parse_saved_procedure(&path)? else {
            continue;
        };
        if procedure.status != ProcedureStatus::Active {
            continue;
        }

        let score = procedure_match_score(instruction, &procedure);
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

pub(crate) fn evaluate_procedure_revision(
    existing: &ParsedProcedure,
    draft: &ProcedureDraft,
) -> ProcedureRevisionAction {
    let same_verification = existing
        .verification
        .trim()
        .eq_ignore_ascii_case(draft.verification.trim());
    let overlapping_steps = draft
        .steps
        .iter()
        .filter(|step| {
            existing
                .steps
                .iter()
                .any(|existing_step| existing_step.eq_ignore_ascii_case(step))
        })
        .count();
    let has_new_steps = draft.steps.iter().any(|step| {
        !existing
            .steps
            .iter()
            .any(|existing_step| existing_step.eq_ignore_ascii_case(step))
    });
    let has_new_pitfalls = draft.pitfalls.iter().any(|pitfall| {
        !existing
            .pitfalls
            .iter()
            .any(|existing_pitfall| existing_pitfall.eq_ignore_ascii_case(pitfall))
    });

    if same_verification && overlapping_steps > 0 {
        if has_new_steps || has_new_pitfalls {
            ProcedureRevisionAction::Refine
        } else {
            ProcedureRevisionAction::Keep
        }
    } else {
        ProcedureRevisionAction::Supersede
    }
}

const REFINED_REUSE_THRESHOLD: u32 = 3;
const SUPERSEDED_REUSE_THRESHOLD: u32 = 2;

pub(crate) fn procedure_revision_quality_gate(
    existing: &ParsedProcedure,
    action: ProcedureRevisionAction,
    has_low_trust_influence: bool,
) -> ProcedureRevisionAction {
    if has_low_trust_influence {
        return ProcedureRevisionAction::Keep;
    }

    match action {
        ProcedureRevisionAction::Refine if existing.reuse_count < REFINED_REUSE_THRESHOLD => {
            ProcedureRevisionAction::Keep
        }
        ProcedureRevisionAction::Supersede if existing.reuse_count < SUPERSEDED_REUSE_THRESHOLD => {
            ProcedureRevisionAction::Keep
        }
        other => other,
    }
}

fn render_procedure_markdown(procedure: &ParsedProcedure) -> String {
    let mut content = String::new();
    content.push_str(&format!("# {}\n\n", procedure.title));
    if let Some(saved_at) = procedure.saved_at {
        content.push_str(&format!("**Saved:** <t:{}>\n", saved_at));
    }
    content.push_str(&format!("**Status:** {}\n", procedure.status.as_str()));
    content.push_str(&format!("**When To Use:** {}\n", procedure.when_to_use));
    content.push_str(&format!("**Verification:** {}\n", procedure.verification));
    content.push_str(&format!("**Reuse Count:** {}\n", procedure.reuse_count));
    content.push_str(&format!(
        "**Revision Count:** {}\n",
        procedure.revision_count
    ));
    if let Some(last_reuse) = procedure.last_verified_reuse_at {
        content.push_str(&format!("**Last Verified Reuse:** <t:{}>\n", last_reuse));
    }
    if let Some(source_task) = &procedure.source_task {
        content.push_str(&format!("**Source Task:** {}\n", source_task));
    }
    if let Some(source_note) = &procedure.source_note {
        content.push_str(&format!("**Source Note:** {}\n", source_note));
    }
    if let Some(source_trajectory) = &procedure.source_trajectory {
        content.push_str(&format!("**Source Trajectory:** {}\n", source_trajectory));
    }
    if let Some(supersedes) = &procedure.supersedes {
        content.push_str(&format!("**Supersedes:** {}\n", supersedes));
    }
    if let Some(superseded_by) = &procedure.superseded_by {
        content.push_str(&format!("**Superseded By:** {}\n", superseded_by));
    }
    if let Some(disabled_reason) = &procedure.disabled_reason {
        content.push_str(&format!("**Disabled Reason:** {}\n", disabled_reason));
    }
    content.push_str("\n---\n\n");
    content.push_str("## Prerequisites\n\n");
    if procedure.prerequisites.is_empty() {
        content.push_str("- None recorded\n\n");
    } else {
        for item in &procedure.prerequisites {
            content.push_str(&format!("- {}\n", item));
        }
        content.push('\n');
    }

    content.push_str("## Steps\n\n");
    if procedure.steps.is_empty() {
        content.push_str("1. Review the current workspace state.\n\n");
    } else {
        for (idx, step) in procedure.steps.iter().enumerate() {
            content.push_str(&format!("{}. {}\n", idx + 1, step));
        }
        content.push('\n');
    }

    content.push_str("## Pitfalls\n\n");
    if procedure.pitfalls.is_empty() {
        content.push_str("- None recorded\n\n");
    } else {
        for pitfall in &procedure.pitfalls {
            content.push_str(&format!("- {}\n", pitfall));
        }
        content.push('\n');
    }

    content.push_str("---\n*Saved by topagent*\n");
    content
}

fn procedure_match_score(instruction: &str, procedure: &ParsedProcedure) -> usize {
    score_text_relevance(instruction, &procedure_haystack(procedure))
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

fn parse_heading(raw: &str) -> Result<String> {
    raw.lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .context("procedure title missing")
}

fn parse_named_field(raw: &str, prefix: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix(prefix).map(str::trim))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_list_section(raw: &str, heading: &str) -> Vec<String> {
    let mut in_section = false;
    let mut items = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == format!("## {heading}") {
            in_section = true;
            continue;
        }

        if in_section && trimmed.starts_with("## ") {
            break;
        }

        if !in_section || trimmed.is_empty() {
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            items.push(item.trim().to_string());
            continue;
        }

        if let Some((index, rest)) = trimmed.split_once(". ") {
            if index.chars().all(|ch| ch.is_ascii_digit()) {
                items.push(rest.trim().to_string());
            }
        }
    }

    items
}

fn parse_saved_timestamp(raw: &str) -> Option<i64> {
    parse_named_field(raw, "**Saved:**").and_then(|value| {
        value
            .trim_start_matches("<t:")
            .trim_end_matches('>')
            .parse()
            .ok()
    })
}

fn merge_unique_items(existing: &[String], incoming: &[String], max_items: usize) -> Vec<String> {
    let mut merged = existing.to_vec();
    for item in incoming {
        if merged
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(item))
        {
            continue;
        }
        merged.push(item.clone());
        if merged.len() >= max_items {
            break;
        }
    }
    merged
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_procedure(reuse_count: u32) -> ParsedProcedure {
        ParsedProcedure {
            filename: "100-test-procedure.md".to_string(),
            title: "Test procedure".to_string(),
            saved_at: Some(100),
            status: ProcedureStatus::Active,
            when_to_use: "Use when testing.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Inspect the code.".to_string(),
                "Run verification.".to_string(),
            ],
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
            path: PathBuf::from(".topagent/procedures/100-test-procedure.md"),
        }
    }

    fn sample_draft_with_new_steps() -> ProcedureDraft {
        ProcedureDraft {
            title: "Test procedure".to_string(),
            when_to_use: "Use when testing.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Inspect the code.".to_string(),
                "Run verification.".to_string(),
                "Add new step.".to_string(),
            ],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test".to_string(),
            source_task: None,
            source_note: None,
            source_trajectory: None,
            supersedes: None,
        }
    }

    fn sample_draft_with_different_verification() -> ProcedureDraft {
        ProcedureDraft {
            title: "Test procedure".to_string(),
            when_to_use: "Use when testing.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec!["Inspect the code.".to_string()],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test -p other".to_string(),
            source_task: None,
            source_note: None,
            source_trajectory: None,
            supersedes: None,
        }
    }

    #[test]
    fn test_quality_gate_keeps_when_low_reuse_refine() {
        let existing = sample_procedure(1);
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(gated, ProcedureRevisionAction::Keep);
    }

    #[test]
    fn test_quality_gate_allows_refine_when_proven_reuse() {
        let existing = sample_procedure(3);
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(gated, ProcedureRevisionAction::Refine);
    }

    #[test]
    fn test_quality_gate_keeps_when_low_reuse_supersede() {
        let existing = sample_procedure(1);
        let draft = sample_draft_with_different_verification();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Supersede);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(gated, ProcedureRevisionAction::Keep);
    }

    #[test]
    fn test_quality_gate_allows_supersede_when_proven_reuse() {
        let existing = sample_procedure(2);
        let draft = sample_draft_with_different_verification();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Supersede);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(gated, ProcedureRevisionAction::Supersede);
    }

    #[test]
    fn test_quality_gate_blocks_revision_on_low_trust() {
        let existing = sample_procedure(5);
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, true);
        assert_eq!(gated, ProcedureRevisionAction::Keep);
    }

    #[test]
    fn test_quality_gate_passes_through_keep() {
        let existing = sample_procedure(0);
        let draft = ProcedureDraft {
            title: "Test procedure".to_string(),
            when_to_use: "Use when testing.".to_string(),
            prerequisites: vec!["Stay in the workspace.".to_string()],
            steps: vec![
                "Inspect the code.".to_string(),
                "Run verification.".to_string(),
            ],
            pitfalls: vec!["Do not skip verification.".to_string()],
            verification: "cargo test".to_string(),
            source_task: None,
            source_note: None,
            source_trajectory: None,
            supersedes: None,
        };
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Keep);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(gated, ProcedureRevisionAction::Keep);
    }

    #[test]
    fn test_quality_gate_threshold_boundary_refine() {
        let mut existing = sample_procedure(2);
        existing.reuse_count = 2;
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Keep,
            "refine at reuse_count=2 should be blocked"
        );
    }

    #[test]
    fn test_quality_gate_threshold_boundary_supersede() {
        let mut existing = sample_procedure(1);
        existing.reuse_count = 1;
        let draft = sample_draft_with_different_verification();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Supersede);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Keep,
            "supersede at reuse_count=1 should be blocked"
        );
    }

    #[test]
    fn test_quality_gate_exact_threshold_refine_allowed() {
        let mut existing = sample_procedure(3);
        existing.reuse_count = 3;
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Refine,
            "refine at reuse_count=3 should be allowed"
        );
    }

    #[test]
    fn test_quality_gate_exact_threshold_supersede_allowed() {
        let mut existing = sample_procedure(2);
        existing.reuse_count = 2;
        let draft = sample_draft_with_different_verification();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Supersede);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Supersede,
            "supersede at reuse_count=2 should be allowed"
        );
    }

    #[test]
    fn test_quality_gate_high_trust_supersede_still_requires_threshold() {
        let mut existing = sample_procedure(0);
        existing.reuse_count = 0;
        let draft = sample_draft_with_different_verification();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Supersede);

        let gated = procedure_revision_quality_gate(&existing, raw, false);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Keep,
            "supersede at reuse_count=0 should be blocked even with high trust"
        );
    }

    #[test]
    fn test_quality_gate_low_trust_overrides_proven_reuse() {
        let mut existing = sample_procedure(5);
        existing.reuse_count = 5;
        let draft = sample_draft_with_new_steps();
        let raw = evaluate_procedure_revision(&existing, &draft);
        assert_eq!(raw, ProcedureRevisionAction::Refine);

        let gated = procedure_revision_quality_gate(&existing, raw, true);
        assert_eq!(
            gated,
            ProcedureRevisionAction::Keep,
            "low trust should always downgrade to Keep"
        );
    }
}

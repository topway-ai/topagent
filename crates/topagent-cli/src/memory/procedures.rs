use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use topagent_core::BehaviorContract;

use super::compact_text_line;
use crate::managed_files::write_managed_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcedureStatus {
    Active,
    Superseded,
}

impl ProcedureStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
        }
    }

    fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "superseded" => Self::Superseded,
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
    pub(crate) source_task: Option<String>,
    pub(crate) source_lesson: Option<String>,
    pub(crate) source_trajectory: Option<String>,
    pub(crate) supersedes: Option<String>,
    pub(crate) superseded_by: Option<String>,
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
    pub(crate) source_lesson: Option<String>,
    pub(crate) source_trajectory: Option<String>,
    pub(crate) supersedes: Option<String>,
}

pub(crate) fn save_procedure(
    procedures_dir: &Path,
    draft: &ProcedureDraft,
) -> Result<(String, PathBuf)> {
    std::fs::create_dir_all(procedures_dir)
        .with_context(|| format!("failed to create {}", procedures_dir.display()))?;

    let timestamp = unix_timestamp_secs();
    let filename = format!("{}-{}.md", timestamp, slugify_title(&draft.title));
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
        source_task: draft.source_task.clone(),
        source_lesson: draft.source_lesson.clone(),
        source_trajectory: draft.source_trajectory.clone(),
        supersedes: draft.supersedes.clone(),
        superseded_by: None,
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
        source_task: parse_named_field(&raw, "**Source Task:**"),
        source_lesson: parse_named_field(&raw, "**Source Lesson:**"),
        source_trajectory: parse_named_field(&raw, "**Source Trajectory:**"),
        supersedes: parse_named_field(&raw, "**Supersedes:**"),
        superseded_by: parse_named_field(&raw, "**Superseded By:**"),
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

pub(crate) fn set_procedure_source_trajectory(path: &Path, trajectory_file: &str) -> Result<()> {
    let Some(mut procedure) = parse_saved_procedure(path)? else {
        return Ok(());
    };
    procedure.source_trajectory = Some(trajectory_file.to_string());
    let content = render_procedure_markdown(&procedure);
    write_managed_file(path, &content, false)?;
    Ok(())
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

fn render_procedure_markdown(procedure: &ParsedProcedure) -> String {
    let mut content = String::new();
    content.push_str(&format!("# {}\n\n", procedure.title));
    if let Some(saved_at) = procedure.saved_at {
        content.push_str(&format!("**Saved:** <t:{}>\n", saved_at));
    }
    content.push_str(&format!("**Status:** {}\n", procedure.status.as_str()));
    content.push_str(&format!("**When To Use:** {}\n", procedure.when_to_use));
    content.push_str(&format!("**Verification:** {}\n", procedure.verification));
    if let Some(source_task) = &procedure.source_task {
        content.push_str(&format!("**Source Task:** {}\n", source_task));
    }
    if let Some(source_lesson) = &procedure.source_lesson {
        content.push_str(&format!("**Source Lesson:** {}\n", source_lesson));
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

fn slugify_title(title: &str) -> String {
    let slug = title
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == ' ' || *ch == '-')
        .collect::<String>()
        .chars()
        .take(48)
        .collect::<String>()
        .replace(' ', "-");
    if slug.is_empty() {
        "procedure".to_string()
    } else {
        slug
    }
}

fn unix_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

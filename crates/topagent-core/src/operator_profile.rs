use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const USER_PROFILE_RELATIVE_PATH: &str = ".topagent/USER.md";
const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
const LEGACY_PREFERENCES_RELATIVE_DIR: &str = ".topagent/topics";
const LEGACY_PREFERENCE_FILE_PREFIX: &str = "operator-preference-";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceCategory {
    ResponseStyle,
    Workflow,
    Tooling,
    Verification,
}

impl PreferenceCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResponseStyle => "response_style",
            Self::Workflow => "workflow",
            Self::Tooling => "tooling",
            Self::Verification => "verification",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "response_style" => Ok(Self::ResponseStyle),
            "workflow" => Ok(Self::Workflow),
            "tooling" => Ok(Self::Tooling),
            "verification" => Ok(Self::Verification),
            other => Err(Error::ToolFailed(format!(
                "operator_profile: unsupported category `{}`",
                other
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorPreferenceRecord {
    pub key: String,
    pub category: PreferenceCategory,
    pub value: String,
    pub rationale: Option<String>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperatorProfile {
    pub preferences: Vec<OperatorPreferenceRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperatorProfileMigrationReport {
    pub migrated_preferences: usize,
    pub removed_legacy_files: usize,
    pub removed_legacy_index_entries: usize,
}

impl OperatorProfile {
    pub fn upsert(&mut self, record: OperatorPreferenceRecord) -> bool {
        let mut existed = false;
        if let Some(existing) = self
            .preferences
            .iter_mut()
            .find(|entry| entry.key == record.key)
        {
            *existing = record;
            existed = true;
        } else {
            self.preferences.push(record);
        }
        self.preferences
            .sort_by(|left, right| left.key.cmp(&right.key));
        existed
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.preferences.len();
        self.preferences.retain(|entry| entry.key != key);
        before != self.preferences.len()
    }

    pub fn is_empty(&self) -> bool {
        self.preferences.is_empty()
    }
}

pub fn user_profile_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(USER_PROFILE_RELATIVE_PATH)
}

pub fn load_operator_profile(workspace_root: &Path) -> Result<OperatorProfile> {
    let path = user_profile_path(workspace_root);
    if !path.exists() {
        return Ok(OperatorProfile::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        Error::ToolFailed(format!(
            "operator_profile: failed to read {}: {}",
            path.display(),
            err
        ))
    })?;
    parse_operator_profile(&raw)
}

pub fn save_operator_profile(workspace_root: &Path, profile: &OperatorProfile) -> Result<()> {
    let path = user_profile_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::ToolFailed(format!(
                "operator_profile: failed to create {}: {}",
                parent.display(),
                err
            ))
        })?;
    }
    std::fs::write(&path, render_operator_profile(profile)).map_err(|err| {
        Error::ToolFailed(format!(
            "operator_profile: failed to write {}: {}",
            path.display(),
            err
        ))
    })
}

pub fn parse_operator_profile(contents: &str) -> Result<OperatorProfile> {
    let mut profile = OperatorProfile::default();
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = Vec::new();

    for line in contents.lines() {
        if let Some(heading) = line.trim().strip_prefix("## ") {
            if let Some(previous) = current_heading.replace(heading.trim().to_string()) {
                sections.push((previous, current_body.join("\n")));
                current_body.clear();
            }
            continue;
        }

        if current_heading.is_some() {
            current_body.push(line.to_string());
        }
    }

    if let Some(previous) = current_heading {
        sections.push((previous, current_body.join("\n")));
    }

    for (key, body) in sections {
        let category = PreferenceCategory::parse(
            &extract_inline_field(&body, "**Category:**").ok_or_else(|| {
                Error::ToolFailed(format!(
                    "operator_profile: missing category for preference `{}`",
                    key
                ))
            })?,
        )?;
        let value = extract_inline_field(&body, "**Preference:**").ok_or_else(|| {
            Error::ToolFailed(format!(
                "operator_profile: missing preference for `{}`",
                key
            ))
        })?;
        let updated_at = extract_saved_timestamp(&body).unwrap_or_default();
        profile.preferences.push(OperatorPreferenceRecord {
            key,
            category,
            value,
            rationale: extract_inline_field(&body, "**Why:**"),
            updated_at,
        });
    }

    profile
        .preferences
        .sort_by(|left, right| left.key.cmp(&right.key));
    Ok(profile)
}

pub fn render_operator_profile(profile: &OperatorProfile) -> String {
    let mut content = String::new();
    content.push_str("# Operator Model\n\n");
    content.push_str(
        "Stable operator preferences and collaboration habits for this workspace. \
Store only durable operator preferences here, not repository facts or task state.\n",
    );

    if profile.preferences.is_empty() {
        content.push_str("\n_No durable operator preferences stored yet._\n");
        return content;
    }

    for record in &profile.preferences {
        content.push_str(&format!("\n## {}\n", record.key));
        content.push_str(&format!("**Category:** {}\n", record.category.as_str()));
        content.push_str(&format!("**Updated:** <t:{}>\n", record.updated_at));
        content.push_str(&format!("**Preference:** {}\n", record.value));
        if let Some(rationale) = &record.rationale {
            content.push_str(&format!("**Why:** {}\n", rationale));
        }
    }
    content
}

pub fn migrate_legacy_operator_preferences(
    workspace_root: &Path,
) -> Result<OperatorProfileMigrationReport> {
    let legacy_dir = workspace_root.join(LEGACY_PREFERENCES_RELATIVE_DIR);
    if !legacy_dir.exists() {
        return Ok(OperatorProfileMigrationReport::default());
    }

    let mut legacy_files = std::fs::read_dir(&legacy_dir)
        .map_err(|err| {
            Error::ToolFailed(format!(
                "operator_profile: failed to read {}: {}",
                legacy_dir.display(),
                err
            ))
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_legacy_preference_file(path))
        .collect::<Vec<_>>();
    legacy_files.sort();

    if legacy_files.is_empty() {
        return Ok(OperatorProfileMigrationReport::default());
    }

    let mut profile = load_operator_profile(workspace_root)?;
    let mut report = OperatorProfileMigrationReport::default();

    for path in &legacy_files {
        let record = parse_legacy_preference_file(path)?;
        profile.upsert(record);
        report.migrated_preferences += 1;
    }

    save_operator_profile(workspace_root, &profile)?;

    for path in &legacy_files {
        std::fs::remove_file(path).map_err(|err| {
            Error::ToolFailed(format!(
                "operator_profile: failed to remove legacy preference {}: {}",
                path.display(),
                err
            ))
        })?;
        report.removed_legacy_files += 1;
    }

    report.removed_legacy_index_entries = remove_legacy_index_entries(workspace_root)?;
    Ok(report)
}

fn remove_legacy_index_entries(workspace_root: &Path) -> Result<usize> {
    let index_path = workspace_root.join(MEMORY_INDEX_RELATIVE_PATH);
    if !index_path.exists() {
        return Ok(0);
    }

    let existing = std::fs::read_to_string(&index_path).map_err(|err| {
        Error::ToolFailed(format!(
            "operator_profile: failed to read {}: {}",
            index_path.display(),
            err
        ))
    })?;
    let lines = existing.lines().collect::<Vec<_>>();
    let kept = lines
        .iter()
        .filter(|line| !is_legacy_index_line(line))
        .copied()
        .collect::<Vec<_>>();
    let removed = lines.len().saturating_sub(kept.len());
    if removed == 0 {
        return Ok(0);
    }

    let mut rewritten = kept.join("\n");
    rewritten.push('\n');
    std::fs::write(&index_path, rewritten).map_err(|err| {
        Error::ToolFailed(format!(
            "operator_profile: failed to write {}: {}",
            index_path.display(),
            err
        ))
    })?;
    Ok(removed)
}

fn is_legacy_index_line(line: &str) -> bool {
    extract_index_field(line, "file").is_some_and(|file| {
        file.starts_with("topics/")
            && file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(LEGACY_PREFERENCE_FILE_PREFIX) && name.ends_with(".md")
                })
    })
}

fn parse_legacy_preference_file(path: &Path) -> Result<OperatorPreferenceRecord> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        Error::ToolFailed(format!(
            "operator_profile: failed to read {}: {}",
            path.display(),
            err
        ))
    })?;

    let key = extract_inline_field(&raw, "**Key:**").ok_or_else(|| {
        Error::ToolFailed(format!(
            "operator_profile: missing key in {}",
            path.display()
        ))
    })?;
    let category = PreferenceCategory::parse(
        &extract_inline_field(&raw, "**Category:**").ok_or_else(|| {
            Error::ToolFailed(format!(
                "operator_profile: missing category in {}",
                path.display()
            ))
        })?,
    )?;
    let value = extract_markdown_section(&raw, "Preference").ok_or_else(|| {
        Error::ToolFailed(format!(
            "operator_profile: missing preference section in {}",
            path.display()
        ))
    })?;

    Ok(OperatorPreferenceRecord {
        key,
        category,
        value,
        rationale: extract_markdown_section(&raw, "Why This Matters"),
        updated_at: extract_saved_timestamp(&raw).unwrap_or_default(),
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

fn extract_saved_timestamp(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let start = line.find("<t:")?;
        let rest = &line[start + 3..];
        let end = rest.find('>')?;
        rest[..end].parse::<u64>().ok()
    })
}

fn extract_index_field(line: &str, field: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if !trimmed.starts_with('-') {
        return None;
    }

    trimmed
        .trim_start_matches('-')
        .trim()
        .split('|')
        .find_map(|part| {
            let (key, value) = part.split_once(':')?;
            (key.trim().eq_ignore_ascii_case(field)).then_some(PathBuf::from(value.trim()))
        })
}

fn is_legacy_preference_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| {
                name.starts_with(LEGACY_PREFERENCE_FILE_PREFIX) && name.ends_with(".md")
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_render_and_parse_operator_profile_round_trip() {
        let profile = OperatorProfile {
            preferences: vec![OperatorPreferenceRecord {
                key: "concise_final_answers".to_string(),
                category: PreferenceCategory::ResponseStyle,
                value: "Keep final responses concise.".to_string(),
                rationale: Some("The operator reviews runs quickly.".to_string()),
                updated_at: 1700000000,
            }],
        };

        let rendered = render_operator_profile(&profile);
        let parsed = parse_operator_profile(&rendered).unwrap();
        assert_eq!(parsed, profile);
    }

    #[test]
    fn test_migrate_legacy_operator_preferences_moves_topics_into_user_md() {
        let temp = TempDir::new().unwrap();
        let topics_dir = temp.path().join(".topagent/topics");
        std::fs::create_dir_all(&topics_dir).unwrap();
        std::fs::write(
            topics_dir.join("operator-preference-concise_final_answers.md"),
            "# Operator Preference: concise final answers\n\n**Key:** concise_final_answers\n**Category:** response_style\n**Updated:** <t:1700000000>\n\n## Preference\n\nKeep final responses concise.\n\n## Why This Matters\n\nThe operator reviews runs quickly.\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".topagent/MEMORY.md"),
            "# TopAgent Memory Index\n\n- topic: operator preference: concise final answers | file: topics/operator-preference-concise_final_answers.md | status: verified | tags: operator, preference, response_style | note: concise\n",
        )
        .unwrap();

        let report = migrate_legacy_operator_preferences(temp.path()).unwrap();
        let profile = load_operator_profile(temp.path()).unwrap();
        let user_md =
            std::fs::read_to_string(temp.path().join(USER_PROFILE_RELATIVE_PATH)).unwrap();
        let index = std::fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.migrated_preferences, 1);
        assert_eq!(profile.preferences.len(), 1);
        assert!(user_md.contains("# Operator Model"));
        assert!(user_md.contains("## concise_final_answers"));
        assert!(!index.contains("operator preference"));
        assert!(!temp
            .path()
            .join(".topagent/topics/operator-preference-concise_final_answers.md")
            .exists());
    }
}

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const USER_PROFILE_RELATIVE_PATH: &str = ".topagent/USER.md";

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

fn extract_inline_field(contents: &str, prefix: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn extract_saved_timestamp(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let start = line.find("<t:")?;
        let rest = &line[start + 3..];
        let end = rest.find('>')?;
        rest[..end].parse::<u64>().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

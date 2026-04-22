use std::path::Path;

use crate::doctor::lint::{
    extract_note_from_index_line, lint_memory_md_content, lint_user_md_content,
};
use crate::doctor::types::{CheckLevel, CheckResult};
use crate::memory::{
    MEMORY_INDEX_RELATIVE_PATH, MEMORY_MD_SIZE_ERROR, MEMORY_MD_SIZE_WARN, USER_MD_SIZE_ERROR,
    USER_MD_SIZE_WARN,
};
use crate::workspace_state::{inspect_workspace_state, WORKSPACE_STATE_RELATIVE_PATH};

const MEMORY_MD_MAX_ENTRIES: usize = 24;
const MEMORY_MD_MAX_NOTE_CHARS: usize = 120;

pub(crate) fn check_workspace_layout(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let state = inspect_workspace_state(workspace);
    if !state.topagent_exists {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Error,
            detail: ".topagent/ directory does not exist".to_string(),
            hint: Some(
                "run a task to auto-create the layout, or check the workspace path".to_string(),
            ),
        });
        return;
    }

    if let Some(err) = state.schema_error {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Error,
            detail: format!("workspace schema marker is unreadable: {}", err),
            hint: Some(format!("inspect {}", WORKSPACE_STATE_RELATIVE_PATH)),
        });
        return;
    }

    if state.missing_required_paths.is_empty() {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Ok,
            detail: format!(
                "schema v{} with all expected workspace-state paths present",
                state
                    .schema_version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ),
            hint: None,
        });
    } else {
        let mut details = Vec::new();
        if !state.missing_required_paths.is_empty() {
            details.push(format!(
                "missing required paths: {}",
                state.missing_required_paths.join(", ")
            ));
        }
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Warning,
            detail: details.join("; "),
            hint: Some(
                "run a task or `topagent memory status` to create the current workspace layout"
                    .to_string(),
            ),
        });
    }
}

pub(crate) fn check_workspace_files(workspace: &Path, checks: &mut Vec<CheckResult>) {
    check_memory_md(workspace, checks);
    check_user_md(workspace, checks);
}

fn check_memory_md(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let path = workspace.join(MEMORY_INDEX_RELATIVE_PATH);
    if !path.exists() {
        checks.push(CheckResult {
            name: "MEMORY.md",
            level: CheckLevel::Warning,
            detail: "file does not exist".to_string(),
            hint: Some("will be auto-created on next task run".to_string()),
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "MEMORY.md",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    let mut level = CheckLevel::Ok;
    let mut details = Vec::new();

    if raw.len() > MEMORY_MD_SIZE_ERROR {
        level = CheckLevel::Error;
        details.push(format!(
            "size {} bytes exceeds error budget ({} bytes)",
            raw.len(),
            MEMORY_MD_SIZE_ERROR
        ));
    } else if raw.len() > MEMORY_MD_SIZE_WARN {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "size {} bytes exceeds warning budget ({} bytes)",
            raw.len(),
            MEMORY_MD_SIZE_WARN
        ));
    }

    let entries: Vec<_> = raw
        .lines()
        .filter(|line| line.trim().starts_with("- "))
        .collect();

    if entries.len() > MEMORY_MD_MAX_ENTRIES {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entries exceeds budget of {}",
            entries.len(),
            MEMORY_MD_MAX_ENTRIES
        ));
    }

    let mut long_notes = 0usize;
    let mut bad_format = 0usize;
    for line in &entries {
        if let Some(note) = extract_note_from_index_line(line) {
            if note.len() > MEMORY_MD_MAX_NOTE_CHARS {
                long_notes += 1;
            }
        } else if !line.contains("|") {
            bad_format += 1;
        }
    }

    if bad_format > 0 {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entry/entries missing pipe-delimited fields",
            bad_format
        ));
    }

    if long_notes > 0 {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entry/entries have notes exceeding {} chars",
            long_notes, MEMORY_MD_MAX_NOTE_CHARS
        ));
    }

    let content_issues = lint_memory_md_content(&raw);
    if !content_issues.is_empty() {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.extend(content_issues);
    }

    let detail = if details.is_empty() {
        format!("{} entries, {} bytes", entries.len(), raw.len())
    } else {
        details.join("; ")
    };

    checks.push(CheckResult {
        name: "MEMORY.md",
        level,
        detail,
        hint: if level != CheckLevel::Ok {
            Some("run `topagent procedure prune` or consolidate to reduce index bulk".to_string())
        } else {
            None
        },
    });
}

fn check_user_md(workspace: &Path, checks: &mut Vec<CheckResult>) {
    use topagent_core::{load_operator_profile, user_profile_path};

    let path = user_profile_path(workspace);
    if !path.exists() {
        checks.push(CheckResult {
            name: "USER.md",
            level: CheckLevel::Ok,
            detail: "not present (optional)".to_string(),
            hint: None,
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "USER.md",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    let mut level = CheckLevel::Ok;
    let mut details = Vec::new();

    if raw.len() > USER_MD_SIZE_ERROR {
        level = CheckLevel::Error;
        details.push(format!(
            "size {} bytes exceeds error budget ({} bytes)",
            raw.len(),
            USER_MD_SIZE_ERROR
        ));
    } else if raw.len() > USER_MD_SIZE_WARN {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "size {} bytes exceeds warning budget ({} bytes)",
            raw.len(),
            USER_MD_SIZE_WARN
        ));
    }

    match load_operator_profile(workspace) {
        Ok(profile) => {
            details.push(format!(
                "{} preference(s) parsed",
                profile.preferences.len()
            ));
        }
        Err(err) => {
            level = CheckLevel::Error;
            details.push(format!("parse error: {}", err));
        }
    }

    let content_issues = lint_user_md_content(&raw);
    if !content_issues.is_empty() {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.extend(content_issues);
    }

    checks.push(CheckResult {
        name: "USER.md",
        level,
        detail: details.join("; "),
        hint: if level != CheckLevel::Ok {
            Some("keep USER.md small: only stable operator preferences, not repo facts or task state".to_string())
        } else {
            None
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MEMORY_INDEX_RELATIVE_PATH;
    use tempfile::TempDir;
    use topagent_core::user_profile_path;

    fn healthy_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();

        crate::workspace_state::ensure_workspace_state(ws).unwrap();

        std::fs::write(
            ws.join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        temp
    }

    #[test]
    fn test_doctor_succeeds_on_healthy_fixture() {
        let temp = healthy_workspace();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks.iter().all(|c| c.level == CheckLevel::Ok));
    }

    #[test]
    fn test_doctor_reports_missing_workspace_layout() {
        let temp = TempDir::new().unwrap();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Error && c.name == "workspace layout"));
    }

    #[test]
    fn test_doctor_reports_broken_workspace_layout() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".topagent")).unwrap();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "workspace layout"));
    }

    #[test]
    fn test_doctor_does_not_mutate_state() {
        let temp = healthy_workspace();
        let memory_path = temp.path().join(MEMORY_INDEX_RELATIVE_PATH);
        let before = std::fs::read_to_string(&memory_path).unwrap();

        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        check_workspace_files(temp.path(), &mut checks);

        let after = std::fs::read_to_string(&memory_path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn test_doctor_memory_md_oversized_warns() {
        let temp = healthy_workspace();
        let big_content = format!(
            "# TopAgent Memory Index\n{}\n",
            "x".repeat(MEMORY_MD_SIZE_WARN + 100)
        );
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &big_content).unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "MEMORY.md"));
    }

    #[test]
    fn test_doctor_memory_md_too_many_entries_warns() {
        let temp = healthy_workspace();
        let mut content = String::from("# TopAgent Memory Index\n\n");
        for i in 0..=MEMORY_MD_MAX_ENTRIES {
            content.push_str(&format!(
                "- title: thing_{i} | file: notes/thing_{i}.md | status: verified | note: ok\n"
            ));
        }
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &content).unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks.iter().any(|c| c.level == CheckLevel::Warning
            && c.name == "MEMORY.md"
            && c.detail.contains("budget")));
    }

    #[test]
    fn test_doctor_memory_md_bad_format_warns() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- no pipe delimiters here just a long line\n",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks.iter().any(|c| c.level == CheckLevel::Warning
            && c.name == "MEMORY.md"
            && c.detail.contains("pipe")));
    }

    #[test]
    fn test_doctor_user_md_oversized_warns() {
        let temp = healthy_workspace();
        let mut content = String::from("# Operator Model\n\n");
        content.push_str(&"x".repeat(USER_MD_SIZE_WARN + 100));
        std::fs::write(user_profile_path(temp.path()), &content).unwrap();
        let mut checks = Vec::new();
        check_user_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "USER.md"));
    }

    #[test]
    fn test_doctor_user_md_parse_error_reports() {
        let temp = healthy_workspace();
        std::fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## bad_entry\n**Not a valid section**\n",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_user_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.name == "USER.md" && c.detail.contains("parse error")));
    }

    #[test]
    fn test_doctor_memory_md_error_size_and_procedure_redirect() {
        let temp = healthy_workspace();
        let big = format!(
            "# TopAgent Memory Index\n\n{}\n",
            "x".repeat(MEMORY_MD_SIZE_ERROR + 100)
        );
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &big).unwrap();

        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        let mem_check = checks.iter().find(|c| c.name == "MEMORY.md").unwrap();
        assert_eq!(mem_check.level, CheckLevel::Error);
        assert!(mem_check.detail.contains("error budget"));
    }

    #[test]
    fn test_doctor_partial_layout_reports_warnings() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".topagent/notes")).unwrap();
        std::fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        let mut layout_checks = Vec::new();
        check_workspace_layout(temp.path(), &mut layout_checks);
        assert!(layout_checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "workspace layout"));
        let detail = layout_checks
            .iter()
            .find(|c| c.name == "workspace layout")
            .unwrap();
        assert!(detail.detail.contains("missing"));
    }
}

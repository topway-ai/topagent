pub(crate) fn lint_memory_md_content(raw: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = raw.to_ascii_lowercase();

    let transient_markers = [
        ("task completed", "transient session outcome"),
        ("task failed", "transient session outcome"),
        ("ran successfully", "transient session outcome"),
        ("just ran", "transient session outcome"),
        ("currently running", "transient session outcome"),
        ("pending approval", "task-local state"),
        ("waiting for", "task-local state"),
        ("todo:", "task-local state"),
        ("fixme:", "task-local state"),
    ];

    let mut transient_count = 0usize;
    for (marker, _label) in &transient_markers {
        if lower.contains(marker) {
            transient_count += 1;
        }
    }
    if transient_count > 0 {
        issues.push(format!(
            "{} transient/task-local marker(s) detected",
            transient_count
        ));
    }

    let transcript_markers = ["assistant:", "user:", "tool_result:", "tool_call:", "```"];
    let mut transcript_count = 0usize;
    for marker in &transcript_markers {
        if lower.contains(marker) {
            transcript_count += 1;
        }
    }
    if transcript_count > 0 {
        issues.push(format!(
            "{} raw transcript marker(s) detected",
            transcript_count
        ));
    }

    let mut procedure_like = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- ") && trimmed.contains("procedure") && trimmed.contains("step") {
            procedure_like += 1;
        }
    }
    if procedure_like > 0 {
        issues.push(format!(
            "{} procedure-like entries belong in .topagent/procedures/",
            procedure_like
        ));
    }

    let verbose_markers = [
        "the agent should",
        "the agent will",
        "the agent can",
        "remember to always",
        "important: make sure",
    ];
    let mut verbose_count = 0usize;
    for marker in &verbose_markers {
        if lower.contains(marker) {
            verbose_count += 1;
        }
    }
    if verbose_count > 0 {
        issues.push(format!(
            "{} verbose/instructional marker(s) detected",
            verbose_count
        ));
    }

    issues
}

pub(crate) fn lint_user_md_content(raw: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = raw.to_ascii_lowercase();

    let forbidden = [
        ("architecture", "repo fact — belongs in notes/"),
        ("runtime behavior", "repo fact — belongs in notes/"),
        ("api endpoint", "repo fact — belongs in notes/"),
        ("database schema", "repo fact — belongs in notes/"),
        ("file structure", "repo fact — belongs in notes/"),
        ("task completed", "transient session outcome"),
        ("just ran", "transient session outcome"),
        ("todo:", "task-local state"),
        ("fixme:", "task-local state"),
    ];

    let mut forbidden_count = 0usize;
    for (marker, _label) in &forbidden {
        if lower.contains(marker) {
            forbidden_count += 1;
        }
    }
    if forbidden_count > 0 {
        issues.push(format!(
            "{} forbidden content marker(s) detected (repo facts or session state)",
            forbidden_count
        ));
    }

    let transcript_markers = ["assistant:", "user:", "tool_result:", "```"];
    let mut transcript_count = 0usize;
    for marker in &transcript_markers {
        if lower.contains(marker) {
            transcript_count += 1;
        }
    }
    if transcript_count > 0 {
        issues.push(format!(
            "{} raw transcript marker(s) detected",
            transcript_count
        ));
    }

    issues
}

pub(crate) fn extract_note_from_index_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with("- ") {
        return None;
    }
    trimmed.split('|').find_map(|part| {
        let (key, value) = part.split_once(':')?;
        (key.trim().eq_ignore_ascii_case("note")).then_some(value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lint_memory_md_flags_transient_content() {
        let content = "# TopAgent Memory Index\n\n- title: deploy | file: notes/deploy.md | status: verified | note: task completed successfully\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transient")));
    }

    #[test]
    fn test_lint_memory_md_flags_transcript_content() {
        let content = "# TopAgent Memory Index\n\n- title: chat | file: notes/chat.md | status: verified | note: assistant: fixed the bug\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transcript")));
    }

    #[test]
    fn test_lint_memory_md_flags_procedure_like_content() {
        let content = "# TopAgent Memory Index\n\n- title: deploy procedure | file: procedures/deploy.md | status: verified | note: step-by-step deployment\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("procedure-like")));
    }

    #[test]
    fn test_lint_memory_md_clean_content_passes() {
        let content = "# TopAgent Memory Index\n\n- title: architecture | file: notes/architecture.md | status: verified | note: service layout\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_lint_user_md_flags_repo_facts() {
        let content = "# Operator Model\n\n## arch_notes\n**Category:** workflow\n**Updated:** <t:1>\n**Preference:** The architecture uses microservices.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.iter().any(|i| i.contains("forbidden")));
    }

    #[test]
    fn test_lint_user_md_flags_transcript_content() {
        let content = "# Operator Model\n\n## notes\n**Category:** workflow\n**Updated:** <t:1>\n**Preference:** assistant: use concise answers.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transcript")));
    }

    #[test]
    fn test_lint_user_md_clean_content_passes() {
        let content = "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.is_empty());
    }
}

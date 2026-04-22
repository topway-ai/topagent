use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_review_rules() -> String {
    let path = repo_root().join("REVIEW_RULES.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read REVIEW_RULES.md: {e}"))
}

fn read_agents() -> String {
    let path = repo_root().join("AGENTS.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read AGENTS.md: {e}"))
}

const FORBIDDEN_PHRASES: &[&str] = &[
    "Keep your current",
    "keep current section",
    "lightly edited for clarity",
    "they are already strong",
    "TODO",
    "TBD",
    "placeholder",
    "checkpoints",
    "generated-tool",
];

const REQUIRED_SECTIONS: &[&str] = &[
    "## Authority",
    "## Glossary",
    "## Preflight Review",
    "## Simplicity Rubric",
    "## Complexity Test",
    "## Performance Guardrails",
    "## Test Requirement",
    "## Documentation Sync",
    "## Rules",
    "## Acceptable Complexity",
    "## Unacceptable Complexity",
    "## Spike Rule",
    "## Exception Handling",
    "## Post-change Review",
];

#[test]
fn review_rules_has_no_placeholder_phrases() {
    let content = read_review_rules();
    for phrase in FORBIDDEN_PHRASES {
        assert!(
            !content.contains(phrase),
            "REVIEW_RULES.md contains placeholder phrase: {phrase:?}"
        );
    }
}

#[test]
fn review_rules_has_required_sections() {
    let content = read_review_rules();
    for section in REQUIRED_SECTIONS {
        assert!(
            content.contains(section),
            "REVIEW_RULES.md is missing required section: {section:?}"
        );
    }
}

#[test]
fn agents_md_points_to_review_rules_as_authoritative() {
    let content = read_agents();
    assert!(
        content.contains("REVIEW_RULES.md"),
        "AGENTS.md must reference REVIEW_RULES.md"
    );
    assert!(
        content.contains("authoritative"),
        "AGENTS.md must describe REVIEW_RULES.md as authoritative"
    );
}

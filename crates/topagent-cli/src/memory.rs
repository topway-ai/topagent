mod briefing;
mod memory_consolidation;
pub(crate) mod observation;
pub(crate) mod procedures;
mod promotion;
pub(crate) mod trajectories;

pub(crate) use self::procedures::{
    disable_procedure, parse_saved_procedure, ParsedProcedure, ProcedureStatus,
};
pub(crate) use self::promotion::promote_verified_task;
pub(crate) use self::trajectories::{
    export_trajectory as write_exported_trajectory, mark_trajectory_ready, parse_saved_trajectory,
    TrajectoryReviewState, TRAJECTORY_EXPORTS_RELATIVE_DIR,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use topagent_core::{
    migrate_legacy_operator_preferences, BehaviorContract, Message, RunTrustContext,
};

use crate::managed_files::write_managed_file;

const MEMORY_ROOT_DIR: &str = ".topagent";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_TOPICS_RELATIVE_DIR: &str = ".topagent/topics";
pub(crate) const MEMORY_LESSONS_RELATIVE_DIR: &str = ".topagent/lessons";
pub(crate) const MEMORY_PLANS_RELATIVE_DIR: &str = ".topagent/plans";
pub(crate) const MEMORY_PROCEDURES_RELATIVE_DIR: &str = ".topagent/procedures";
pub(crate) const MEMORY_TRAJECTORIES_RELATIVE_DIR: &str = ".topagent/trajectories";
pub(crate) const MEMORY_OBSERVATIONS_RELATIVE_DIR: &str = ".topagent/observations";
const AUTO_PROMOTED_TAG: &str = "curated";

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
    pub operator_prompt: Option<String>,
    pub stats: MemoryPromptStats,
    pub trust_context: RunTrustContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MemoryPromptStats {
    pub total_prompt_bytes: usize,
    pub index_prompt_bytes: usize,
    pub transcript_prompt_bytes: usize,
    pub loaded_operator_items: Vec<String>,
    pub loaded_items: Vec<String>,
    pub loaded_procedure_files: Vec<String>,
    pub transcript_snippets: usize,
    pub observation_hints_used: usize,
    pub provenance_notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMemory {
    workspace_root: PathBuf,
    index_path: PathBuf,
    topics_dir: PathBuf,
    lessons_dir: PathBuf,
    plans_dir: PathBuf,
    procedures_dir: PathBuf,
    trajectories_dir: PathBuf,
    observations_dir: PathBuf,
}

impl WorkspaceMemory {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self {
            index_path: workspace_root.join(MEMORY_INDEX_RELATIVE_PATH),
            topics_dir: workspace_root.join(MEMORY_TOPICS_RELATIVE_DIR),
            lessons_dir: workspace_root.join(MEMORY_LESSONS_RELATIVE_DIR),
            plans_dir: workspace_root.join(MEMORY_PLANS_RELATIVE_DIR),
            procedures_dir: workspace_root.join(MEMORY_PROCEDURES_RELATIVE_DIR),
            trajectories_dir: workspace_root.join(MEMORY_TRAJECTORIES_RELATIVE_DIR),
            observations_dir: workspace_root.join(MEMORY_OBSERVATIONS_RELATIVE_DIR),
            workspace_root,
        }
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn observations_dir(&self) -> &Path {
        &self.observations_dir
    }

    pub(crate) fn ensure_layout(&self) -> Result<()> {
        std::fs::create_dir_all(&self.topics_dir)
            .with_context(|| format!("failed to create {}", self.topics_dir.display()))?;
        std::fs::create_dir_all(&self.lessons_dir)
            .with_context(|| format!("failed to create {}", self.lessons_dir.display()))?;
        std::fs::create_dir_all(&self.plans_dir)
            .with_context(|| format!("failed to create {}", self.plans_dir.display()))?;
        std::fs::create_dir_all(&self.procedures_dir)
            .with_context(|| format!("failed to create {}", self.procedures_dir.display()))?;
        std::fs::create_dir_all(&self.trajectories_dir)
            .with_context(|| format!("failed to create {}", self.trajectories_dir.display()))?;
        std::fs::create_dir_all(&self.observations_dir)
            .with_context(|| format!("failed to create {}", self.observations_dir.display()))?;
        migrate_legacy_operator_preferences(&self.workspace_root)
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

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

    pub(crate) fn build_prompt(
        &self,
        instruction: &str,
        transcript_messages: Option<&[Message]>,
    ) -> Result<MemoryPrompt> {
        briefing::build_prompt(self, instruction, transcript_messages)
    }
}

fn score_text_relevance(instruction: &str, haystack: &str) -> usize {
    let instruction_tokens = tokenize(instruction);
    if instruction_tokens.is_empty() {
        return 0;
    }

    let mut score = tokenize(haystack).intersection(&instruction_tokens).count();
    let lower_instruction = instruction.to_ascii_lowercase();
    let lower_haystack = haystack.to_ascii_lowercase();
    if lower_haystack.contains(&lower_instruction) || lower_instruction.contains(&lower_haystack) {
        score += 2;
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

fn normalize_memory_file(file: &str) -> String {
    file.trim()
        .trim_start_matches("./")
        .trim_start_matches(".topagent/")
        .to_string()
}

fn display_memory_file(file: &str) -> String {
    let normalized = normalize_memory_file(file);
    if normalized.starts_with("topics/")
        || normalized.starts_with("lessons/")
        || normalized.starts_with("plans/")
        || normalized.starts_with("procedures/")
    {
        normalized
    } else {
        format!("topics/{normalized}")
    }
}

fn allowed_memory_prefix(contract: &BehaviorContract, normalized: &str) -> bool {
    let topic_prefix = format!("{}/", contract.memory.topic_file_relative_dir);
    if normalized.starts_with(&topic_prefix) {
        return true;
    }

    contract
        .memory
        .archival_relative_dirs
        .iter()
        .map(|dir| format!("{dir}/"))
        .any(|prefix| normalized.starts_with(&prefix))
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

fn compact_note(parts: &[Option<String>], max_chars: usize) -> String {
    let mut compact = String::new();
    for part in parts.iter().flatten() {
        if part.trim().is_empty() {
            continue;
        }
        if !compact.is_empty() {
            compact.push_str("; ");
        }
        compact.push_str(part.trim());
    }
    compact_text_line(&compact, max_chars)
}

fn artifact_filename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|name| name.to_str())
}

#[cfg(test)]
mod tests {
    use super::procedures::{save_procedure, ProcedureDraft};
    use super::promotion::TaskPromotionReport;
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use topagent_core::{
        tools::default_tools, Agent, ExecutionContext, InfluenceMode, Message, Plan,
        ProviderResponse, Role, RunTrustContext, RuntimeOptions, ScriptedProvider, SecretRegistry,
        SourceKind, SourceLabel, TaskMode, TaskResult, ToolTraceStep, VerificationCommand,
        WorkspaceCheckpointStore,
    };

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

    fn write_lesson(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_LESSONS_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_plan(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_PLANS_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_procedure(workspace: &Path, name: &str, body: &str) {
        let path = workspace.join(MEMORY_PROCEDURES_RELATIVE_DIR).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn verified_task_result() -> TaskResult {
        TaskResult::new("Unified the model control path and reran the CLI test suite.".to_string())
            .with_files_changed(vec![
                "crates/topagent-cli/src/config.rs".to_string(),
                "crates/topagent-cli/src/service/mod.rs".to_string(),
                "crates/topagent-cli/src/service/lifecycle.rs".to_string(),
            ])
            .with_verification_command(VerificationCommand {
                command: "cargo test -p topagent-cli".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            })
    }

    fn strong_verified_task_result(output: &str) -> TaskResult {
        strong_verified_task_result_with_command(output, "cargo test -p topagent-cli")
    }

    fn strong_verified_task_result_with_command(output: &str, command: &str) -> TaskResult {
        TaskResult::new(
            "Hardened the approval mailbox compaction flow and reran the CLI test suite."
                .to_string(),
        )
        .with_files_changed(vec![
            "crates/topagent-core/src/approval.rs".to_string(),
            "crates/topagent-core/src/run_state.rs".to_string(),
        ])
        .with_tool_trace(vec![
            ToolTraceStep {
                tool_name: "read".to_string(),
                summary: "read crates/topagent-core/src/approval.rs".to_string(),
            },
            ToolTraceStep {
                tool_name: "edit".to_string(),
                summary: "edit crates/topagent-core/src/approval.rs".to_string(),
            },
            ToolTraceStep {
                tool_name: "bash".to_string(),
                summary: format!("verification: {command}"),
            },
        ])
        .with_verification_command(VerificationCommand {
            command: command.to_string(),
            output: format!("first pass failed: {output}"),
            exit_code: 1,
            succeeded: false,
        })
        .with_verification_command(VerificationCommand {
            command: command.to_string(),
            output: format!("final pass ok: {output}"),
            exit_code: 0,
            succeeded: true,
        })
    }

    fn strong_plan() -> Plan {
        let mut plan = Plan::new();
        plan.add_item("Inspect the approval mailbox and compaction flow".to_string());
        plan.add_item("Preserve pending approval anchors through the state transition".to_string());
        plan.add_item("Rerun the CLI verification and confirm the proof stays honest".to_string());
        plan
    }

    fn strong_plan_with_extra_item() -> Plan {
        let mut plan = strong_plan();
        plan.add_item("Clear stale transcript state before finishing the workflow".to_string());
        plan
    }

    fn low_trust_transcript_source() -> SourceLabel {
        SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "2 prior transcript snippet(s)",
        )
    }

    fn create_temp_crate() -> (TempDir, ExecutionContext) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"[package]
name = "memory_lifecycle_fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n",
        )
        .unwrap();

        (temp, ExecutionContext::new(root))
    }

    fn assistant_message(text: &str) -> ProviderResponse {
        ProviderResponse::Message(Message {
            role: Role::Assistant,
            content: topagent_core::Content::Text {
                text: text.to_string(),
            },
        })
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ProviderResponse {
        ProviderResponse::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    fn write_lib_call(id: &str, content: &str) -> ProviderResponse {
        tool_call(
            id,
            "write",
            serde_json::json!({
                "path": "src/lib.rs",
                "content": content,
            }),
        )
    }

    fn cargo_check_call(id: &str) -> ProviderResponse {
        tool_call(
            id,
            "bash",
            serde_json::json!({
                "command": "cargo check --offline",
            }),
        )
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
        let report = memory.consolidate_memory_if_needed().unwrap();
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

        assert!(
            prompt.stats.index_prompt_bytes <= memory_contract().memory.max_index_prompt_bytes + 80
        );
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

        assert_eq!(prompt.stats.loaded_items, vec!["security".to_string()]);
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

    #[test]
    fn test_build_prompt_keeps_operator_model_separate_from_workspace_memory() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".topagent")).unwrap();
        fs::write(
            temp.path().join(".topagent/USER.md"),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: runtime details\n",
        );
        write_topic(
            temp.path(),
            "architecture.md",
            "# Architecture\nruntime details",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt(
                "inspect runtime architecture and keep the answer concise",
                None,
            )
            .unwrap();

        assert!(prompt
            .operator_prompt
            .as_deref()
            .unwrap()
            .contains("concise final answers"));
        assert!(prompt.prompt.as_deref().unwrap().contains("# Architecture"));
        assert!(!prompt
            .prompt
            .as_deref()
            .unwrap()
            .contains("concise final answers"));
    }

    #[test]
    fn test_build_prompt_marks_transcript_snippets_as_low_trust() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let messages = vec![
            Message::user("Remember this copied issue body."),
            Message::assistant("Stored the copied issue body."),
        ];

        let prompt = memory
            .build_prompt(
                "what copied issue body did I mention earlier?",
                Some(&messages),
            )
            .unwrap();

        assert!(prompt.trust_context.has_low_trust_action_influence());
        assert!(prompt
            .trust_context
            .low_trust_action_summary(2)
            .unwrap_or_default()
            .contains("prior transcript"));
    }

    #[test]
    fn test_transcript_prompt_stats_stay_capped_under_growth() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let mut messages = Vec::new();
        for idx in 0..20 {
            messages.push(Message::user(format!(
                "approval mailbox snippet {idx} with matching keywords and extra explanation"
            )));
            messages.push(Message::assistant(format!(
                "acknowledged approval mailbox snippet {idx} with more detail"
            )));
        }

        let prompt = memory
            .build_prompt(
                "what did we say earlier about approval mailbox snippets before the restart?",
                Some(&messages),
            )
            .unwrap();

        assert!(
            prompt.stats.transcript_snippets <= memory_contract().memory.max_transcript_snippets
        );
        assert!(
            prompt.stats.transcript_prompt_bytes
                <= memory_contract().memory.max_transcript_prompt_bytes
        );
        assert_eq!(prompt.trust_context.low_trust_sources().len(), 1);
        assert!(prompt
            .trust_context
            .low_trust_action_summary(2)
            .unwrap_or_default()
            .contains("prior transcript"));
    }

    #[test]
    fn test_build_prompt_never_loads_trajectory_artifacts() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: approval flow | file: topics/approval.md | status: verified | note: current repo approval flow\n",
        );
        write_topic(temp.path(), "approval.md", "# Approval\nrepo flow");
        fs::create_dir_all(temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR)).unwrap();
        fs::write(
            temp.path()
                .join(MEMORY_TRAJECTORIES_RELATIVE_DIR)
                .join("ignored.json"),
            "{\"task_intent\":\"ignored\"}",
        )
        .unwrap();

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt("inspect the approval flow", None)
            .unwrap();

        assert!(prompt.prompt.as_deref().unwrap().contains("# Approval"));
        assert!(!prompt.prompt.as_deref().unwrap().contains("ignored"));
    }

    #[test]
    fn test_build_prompt_ignores_many_superseded_procedures_without_growing_working_set() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        for idx in 0..12 {
            write_procedure(
                temp.path(),
                &format!("100-old-{idx}.md"),
                &format!(
                    "# Approval Mailbox Procedure {idx}\n\n**Saved:** <t:{}>\n**Status:** superseded\n**When To Use:** Old approval mailbox compaction workflow.\n**Verification:** cargo test -p topagent-core approval\n**Superseded By:** .topagent/procedures/200-approval-new.md\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Inspect the old flow.\n\n## Pitfalls\n\n- Do not keep using this procedure.\n",
                    1700002000 + idx
                ),
            );
        }
        write_procedure(
            temp.path(),
            "200-approval-new.md",
            "# Approval Mailbox Procedure\n\n**Saved:** <t:1700002500>\n**Status:** active\n**When To Use:** Approval mailbox compaction with pending anchor retention.\n**Verification:** cargo test -p topagent-core approval\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Preserve pending approval anchors.\n\n## Pitfalls\n\n- Do not drop pending approvals.\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.consolidate_memory_if_needed().unwrap();

        let prompt = memory
            .build_prompt("approval mailbox compaction", None)
            .unwrap();

        assert_eq!(prompt.stats.loaded_procedure_files.len(), 1);
        assert_eq!(
            prompt.stats.loaded_procedure_files,
            vec![".topagent/procedures/200-approval-new.md".to_string()]
        );
        assert_eq!(prompt.stats.loaded_items.len(), 1);
    }

    #[test]
    fn test_repeat_task_prompt_working_set_stays_flat_as_irrelevant_artifacts_grow() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();

        let relevant = [
            ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Inspect the mailbox.".to_string(),
                    "Compact safely.".to_string(),
                ],
                pitfalls: vec!["Do not drop pending approvals.".to_string()],
                verification: "cargo test -p topagent-core approval".to_string(),
                source_task: Some("approval mailbox compaction".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
            ProcedureDraft {
                title: "Approval mailbox restore flow".to_string(),
                when_to_use: "Use for restoring approval mailbox state.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Restore the checkpoint.".to_string(),
                    "Rebuild anchors.".to_string(),
                ],
                pitfalls: vec!["Do not keep stale transcript state.".to_string()],
                verification: "cargo test -p topagent-cli telegram".to_string(),
                source_task: Some("approval mailbox restore".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
        ];

        for procedure in relevant {
            save_procedure(&memory.procedures_dir, &procedure).unwrap();
        }
        memory.consolidate_memory_if_needed().unwrap();

        let baseline = memory
            .build_prompt("repair approval mailbox compaction and restore flow", None)
            .unwrap();

        for idx in 0..25 {
            save_procedure(
                &memory.procedures_dir,
                &ProcedureDraft {
                    title: format!("Irrelevant workflow {idx}"),
                    when_to_use: "Use for unrelated UI polish.".to_string(),
                    prerequisites: vec!["Stay within the workspace.".to_string()],
                    steps: vec!["Tweak an unrelated path.".to_string()],
                    pitfalls: vec!["Do not conflate with approval flow.".to_string()],
                    verification: "cargo test -p topagent-cli".to_string(),
                    source_task: Some("irrelevant ui polish".to_string()),
                    source_lesson: None,
                    source_trajectory: None,
                    supersedes: None,
                },
            )
            .unwrap();
            fs::write(
                temp.path()
                    .join(MEMORY_TRAJECTORIES_RELATIVE_DIR)
                    .join(format!("ignored-{idx}.json")),
                "{\"task_intent\":\"ignored trajectory\"}",
            )
            .unwrap();
            write_lesson(
                temp.path(),
                &format!("1700003000-lesson-{idx}.md"),
                &format!(
                    "# Irrelevant Lesson {idx}\n\n**Saved:** <t:{}>\n\n---\n\n## What Changed\n\nUpdated an unrelated visual theme.\n\n## What Was Learned\n\nKeep decorative banner tweaks separate from backend workflows.\n\n---\n*Saved by topagent*\n",
                    1700003000 + idx
                ),
            );
        }
        memory.consolidate_memory_if_needed().unwrap();

        let grown = memory
            .build_prompt("repair approval mailbox compaction and restore flow", None)
            .unwrap();

        assert_eq!(
            baseline.stats.loaded_procedure_files,
            grown.stats.loaded_procedure_files
        );
        assert_eq!(baseline.stats.loaded_items, grown.stats.loaded_items);
        assert!(
            grown.stats.index_prompt_bytes <= memory_contract().memory.max_index_prompt_bytes + 80
        );
        assert!(!grown
            .prompt
            .as_deref()
            .unwrap_or_default()
            .contains("ignored trajectory"));
    }

    #[test]
    fn test_promote_verified_task_creates_lesson_procedure_and_trajectory() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair the approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("super-secret-output-value"),
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();

        assert!(report.lesson_file.is_some());
        assert!(report.procedure_file.is_some());
        assert!(report.trajectory_file.is_some());

        let lesson_path = temp.path().join(report.lesson_file.unwrap());
        let procedure_path = temp.path().join(report.procedure_file.unwrap());
        let trajectory_path = temp.path().join(report.trajectory_file.unwrap());
        let memory_index =
            fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();
        let lesson = fs::read_to_string(&lesson_path).unwrap();
        let procedure = fs::read_to_string(&procedure_path).unwrap();
        let trajectory = fs::read_to_string(&trajectory_path).unwrap();

        assert!(lesson_path.is_file());
        assert!(procedure_path.is_file());
        assert!(trajectory_path.is_file());
        assert!(memory_index.contains("file: lessons/"));
        assert!(memory_index.contains("file: procedures/"));
        assert!(lesson.starts_with("# "));
        assert!(procedure.contains("## Steps"));
        assert!(procedure.contains("**Source Trajectory:** .topagent/trajectories/"));
        assert_ne!(lesson.lines().next(), procedure.lines().next());
        assert!(trajectory.contains("\"tool_sequence\""));
        assert!(trajectory.contains("\"verification\""));
        assert!(trajectory.contains("\"stored_outputs\": false"));
        assert!(!trajectory.contains("super-secret-output-value"));
        assert!(!temp.path().join(".topagent/USER.md").exists());
    }

    #[test]
    fn test_promote_verified_task_blocks_procedure_under_low_trust_influence() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let task_result = strong_verified_task_result("trusted local verification")
            .with_source_labels(vec![low_trust_transcript_source()]);

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair the approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &task_result,
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();

        assert!(report.lesson_file.is_some());
        assert!(report.procedure_file.is_none());
        assert!(report.trajectory_file.is_some());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("Procedure promotion blocked")));
    }

    #[test]
    fn test_promote_verified_task_skips_without_passing_verification() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let mut failed = verified_task_result();
        failed = failed.with_verification_command(VerificationCommand {
            command: "cargo test -p topagent-cli".to_string(),
            output: "fail".to_string(),
            exit_code: 1,
            succeeded: false,
        });

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Unify the model control path and rerun CLI tests",
            TaskMode::PlanAndExecute,
            &failed,
            &Plan::new(),
            false,
            &[],
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_skips_trivial_verified_work() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();
        let trivial = TaskResult::new("Updated one file and reran one verification.".to_string())
            .with_files_changed(vec!["README.md".to_string()])
            .with_verification_command(VerificationCommand {
                command: "cargo test -p topagent-cli".to_string(),
                output: "ok".to_string(),
                exit_code: 0,
                succeeded: true,
            });
        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Update one README line and rerun the CLI test",
            TaskMode::PlanAndExecute,
            &trivial,
            &Plan::new(),
            false,
            &[],
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_skips_when_memory_was_already_written() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair the approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("already saved elsewhere"),
            &strong_plan(),
            true,
            &[],
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    #[test]
    fn test_promote_verified_task_reuses_matching_loaded_procedure() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let first = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("first output"),
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();
        let prompt = memory
            .build_prompt("repair approval mailbox compaction workflow", None)
            .unwrap();
        let second = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow with pending anchor retention",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("second output"),
            &strong_plan(),
            false,
            &prompt.stats.loaded_procedure_files,
        )
        .unwrap();

        let first_procedure = first.procedure_file.unwrap();
        let second_procedure = second.procedure_file.unwrap();
        assert_eq!(second.superseded_procedure_file, None);
        assert_eq!(second_procedure, first_procedure);

        let reused = parse_saved_procedure(&temp.path().join(&first_procedure))
            .unwrap()
            .unwrap();
        assert_eq!(reused.status, ProcedureStatus::Active);
        assert_eq!(reused.reuse_count, 1);
        assert_eq!(reused.revision_count, 0);
        assert!(prompt
            .stats
            .loaded_procedure_files
            .contains(&first_procedure));
    }

    #[test]
    fn test_promote_verified_task_refines_loaded_procedure_after_verified_reuse() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let first = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("first output"),
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();
        let prompt = memory
            .build_prompt("repair approval mailbox compaction workflow", None)
            .unwrap();
        let second = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("second output"),
            &strong_plan_with_extra_item(),
            false,
            &prompt.stats.loaded_procedure_files,
        )
        .unwrap();

        let procedure_path = temp.path().join(first.procedure_file.unwrap());
        let refined = parse_saved_procedure(&procedure_path).unwrap().unwrap();
        assert_eq!(
            second.procedure_file,
            Some(".topagent/procedures/".to_string() + &refined.filename)
        );
        assert_eq!(refined.status, ProcedureStatus::Active);
        assert_eq!(refined.reuse_count, 1);
        assert_eq!(refined.revision_count, 1);
        assert!(refined
            .steps
            .iter()
            .any(|step| step.contains("Clear stale transcript state")));
    }

    #[test]
    fn test_promote_verified_task_supersedes_loaded_procedure_when_verification_changes() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let options = RuntimeOptions::default();

        let first = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("first output"),
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();
        let prompt = memory
            .build_prompt("repair approval mailbox compaction workflow", None)
            .unwrap();
        let second = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow with restore verification",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result_with_command(
                "second output",
                "cargo test -p topagent-core",
            ),
            &strong_plan(),
            false,
            &prompt.stats.loaded_procedure_files,
        )
        .unwrap();

        let first_procedure = first.procedure_file.unwrap();
        let second_procedure = second.procedure_file.unwrap();
        assert_eq!(
            second.superseded_procedure_file.as_deref(),
            Some(first_procedure.as_str())
        );
        assert_ne!(first_procedure, second_procedure);

        let old = parse_saved_procedure(&temp.path().join(&first_procedure))
            .unwrap()
            .unwrap();
        let new = parse_saved_procedure(&temp.path().join(&second_procedure))
            .unwrap()
            .unwrap();
        assert_eq!(old.status, ProcedureStatus::Superseded);
        assert_eq!(new.status, ProcedureStatus::Active);
    }

    #[test]
    fn test_build_prompt_loads_only_small_relevant_procedure_subset() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();

        let procedures = [
            ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Inspect the mailbox.".to_string(),
                    "Compact safely.".to_string(),
                ],
                pitfalls: vec!["Do not drop pending approvals.".to_string()],
                verification: "cargo test -p topagent-core approval".to_string(),
                source_task: Some("approval mailbox compaction".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
            ProcedureDraft {
                title: "Approval mailbox restore flow".to_string(),
                when_to_use: "Use for restoring approval mailbox state.".to_string(),
                prerequisites: vec!["Stay within the workspace.".to_string()],
                steps: vec![
                    "Restore the checkpoint.".to_string(),
                    "Rebuild anchors.".to_string(),
                ],
                pitfalls: vec!["Do not keep stale transcript state.".to_string()],
                verification: "cargo test -p topagent-cli telegram".to_string(),
                source_task: Some("approval mailbox restore".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
            ProcedureDraft {
                title: "Operator response tone guide".to_string(),
                when_to_use: "Use when editing operator-facing prose.".to_string(),
                prerequisites: vec!["Match repo tone.".to_string()],
                steps: vec!["Keep answers concise.".to_string()],
                pitfalls: vec!["Do not add fluff.".to_string()],
                verification: "cargo test -p topagent-cli".to_string(),
                source_task: Some("operator response tone".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
        ];

        for procedure in procedures {
            save_procedure(&memory.procedures_dir, &procedure).unwrap();
        }
        memory.consolidate_memory_if_needed().unwrap();

        let prompt = memory
            .build_prompt("repair approval mailbox compaction and restore flow", None)
            .unwrap();
        assert_eq!(prompt.stats.loaded_items.len(), 2);
        assert!(prompt
            .stats
            .loaded_items
            .contains(&"Approval mailbox compaction playbook".to_string()));
        assert!(prompt
            .stats
            .loaded_items
            .contains(&"Approval mailbox restore flow".to_string()));
        assert!(!prompt
            .stats
            .loaded_items
            .contains(&"Operator response tone guide".to_string()));
    }

    #[test]
    fn test_build_prompt_skips_superseded_procedure_entries() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_procedure(
            temp.path(),
            "100-approval-old.md",
            "# Old Approval Mailbox Procedure\n\n**Saved:** <t:100>\n**Status:** superseded\n**When To Use:** Use for old approval mailbox compaction work.\n**Verification:** cargo test -p topagent-core approval\n**Superseded By:** .topagent/procedures/200-approval-new.md\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Inspect the old flow.\n\n## Pitfalls\n\n- Do not use this anymore.\n",
        );
        write_procedure(
            temp.path(),
            "200-approval-new.md",
            "# New Approval Mailbox Procedure\n\n**Saved:** <t:200>\n**Status:** active\n**When To Use:** Use for approval mailbox compaction with pending anchor retention.\n**Verification:** cargo test -p topagent-core approval\n\n---\n\n## Prerequisites\n\n- Stay inside the workspace.\n\n## Steps\n\n1. Preserve pending anchors.\n\n## Pitfalls\n\n- Do not drop pending approvals.\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.consolidate_memory_if_needed().unwrap();

        let prompt = memory
            .build_prompt("approval mailbox compaction", None)
            .unwrap();
        let rendered = prompt.prompt.unwrap();
        assert!(rendered.contains("New Approval Mailbox Procedure"));
        assert!(!rendered.contains("Old Approval Mailbox Procedure"));
    }

    #[test]
    fn test_promote_verified_task_redacts_registered_secrets_from_saved_artifacts() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let mut secrets = SecretRegistry::new();
        secrets.register("super-secret-output-value");
        let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_secrets(secrets);
        let options = RuntimeOptions::default();

        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "Repair approval mailbox compaction workflow",
            TaskMode::PlanAndExecute,
            &strong_verified_task_result("super-secret-output-value"),
            &strong_plan(),
            false,
            &[],
        )
        .unwrap();

        let lesson = fs::read_to_string(temp.path().join(report.lesson_file.unwrap())).unwrap();
        let procedure =
            fs::read_to_string(temp.path().join(report.procedure_file.unwrap())).unwrap();
        let trajectory =
            fs::read_to_string(temp.path().join(report.trajectory_file.unwrap())).unwrap();

        assert!(!lesson.contains("super-secret-output-value"));
        assert!(!procedure.contains("super-secret-output-value"));
        assert!(!trajectory.contains("super-secret-output-value"));
        assert!(trajectory.contains("[REDACTED]") || !trajectory.contains("first pass failed"));
    }

    #[test]
    fn test_consolidate_promotes_saved_lesson_with_absolute_date() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_lesson(
            temp.path(),
            "1700000000-approval-mailbox.md",
            "# Approval Mailbox\n\n**Saved:** <t:1700000000>\n\n---\n\n## What Changed\n\nAdded mailbox persistence within a live run.\n\n## What Was Learned\n\nPending approvals must stay visible after compaction.\n\n## Reuse Next Time\n\nKeep the mailbox as the canonical approval artifact.\n\n---\n*Saved by topagent*\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_lessons, 1);
        assert!(report.normalized_dates >= 1);
        assert!(rewritten.contains("topic: Approval Mailbox"));
        assert!(rewritten.contains("file: lessons/1700000000-approval-mailbox.md"));
        assert!(rewritten.contains("saved 2023-11-14"));
    }

    #[test]
    fn test_consolidate_prefers_verified_entry_and_prunes_stale_duplicate() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: approval mailbox | file: topics/approval.md | status: verified | tags: approval | note: operator approval gates runtime mutations\n- topic: approval mailbox | file: topics/approval.md | status: stale | tags: approval | note: runtime still allows mutation without approval\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.contradictions_resolved, 1);
        assert_eq!(report.stale_entries_pruned, 1);
        assert_eq!(rewritten.matches("topic: approval mailbox").count(), 1);
        assert!(rewritten.contains("status: verified"));
        assert!(!rewritten.contains("status: stale"));
    }

    #[test]
    fn test_consolidate_merges_saved_plan_duplicates_by_title() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");
        write_plan(
            temp.path(),
            "1700000100-release-flow.md",
            "# Release Flow\n\n**Saved:** <t:1700000100>\n\n**Task:** ship the current patch safely\n\n---\n\n## Plan Items\n\n- [>] run cargo fmt --all --check\n- [ ] run cargo test --workspace\n\n---\n*Saved by topagent*\n",
        );
        write_plan(
            temp.path(),
            "1700000200-release-flow.md",
            "# Release Flow\n\n**Saved:** <t:1700000200>\n\n**Task:** ship the current patch safely\n\n---\n\n## Plan Items\n\n- [>] run cargo clippy --workspace --all-targets -- -D warnings\n- [ ] run cargo test --workspace\n\n---\n*Saved by topagent*\n",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(report.promoted_plans, 1);
        assert_eq!(report.merged_entries, 1);
        assert_eq!(rewritten.matches("topic: Release Flow").count(), 1);
        assert!(rewritten.contains("file: plans/1700000200-release-flow.md"));
        assert!(rewritten.contains("saved 2023-11-14"));
    }

    #[test]
    fn test_consolidate_prunes_curated_lessons_to_policy_limit() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");

        for idx in 0..(memory_contract().memory.max_curated_lessons + 2) {
            let timestamp = 1700001000 + idx as i64;
            write_lesson(
                temp.path(),
                &format!("{timestamp}-lesson-{idx}.md"),
                &format!(
                    "# Lesson {idx}\n\n**Saved:** <t:{timestamp}>\n\n---\n\n## What Changed\n\nUpdated item {idx}.\n\n## What Was Learned\n\nLesson {idx} remains useful for future runs.\n\n---\n*Saved by topagent*\n"
                ),
            );
        }

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let report = memory.consolidate_memory_if_needed().unwrap();
        let rewritten = fs::read_to_string(temp.path().join(MEMORY_INDEX_RELATIVE_PATH)).unwrap();

        assert_eq!(
            rewritten.matches("| file: lessons/").count(),
            memory_contract().memory.max_curated_lessons
        );
        assert_eq!(
            report.promoted_lessons,
            memory_contract().memory.max_curated_lessons
        );
        assert!(report.pruned_entries >= 2);
    }

    #[test]
    fn test_actual_low_trust_verified_run_blocks_procedure_promotion() {
        let (temp, ctx) = create_temp_crate();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let mut trust = RunTrustContext::default();
        trust.add_source(low_trust_transcript_source());
        let ctx = ctx.with_run_trust_context(trust);
        let options = RuntimeOptions::default();
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call("read", "read", serde_json::json!({"path": "src/lib.rs"})),
                write_lib_call("write", "pub fn answer() -> u32 {\n    99\n}\n"),
                cargo_check_call("verify"),
                assistant_message("done after transcript-derived verification"),
            ])),
            default_tools().into_inner(),
            options.clone(),
        );

        let result = agent
            .run(&ctx, "apply the transcript-derived fix and verify")
            .unwrap();
        assert!(result.contains("Low-trust content influenced this run"));

        let task_result = agent
            .last_task_result()
            .cloned()
            .expect("expected a structured task result");
        let report = promote_verified_task(
            &memory,
            &ctx,
            &options,
            "apply the transcript-derived fix and verify",
            agent.task_mode(),
            &task_result,
            &strong_plan(),
            agent.durable_memory_written_this_run(),
            &[],
        )
        .unwrap();

        assert!(task_result.final_verification_passed());
        assert!(task_result
            .source_labels()
            .iter()
            .any(|label| label.kind == SourceKind::TranscriptPrior));
        assert!(report.lesson_file.is_some());
        assert!(report.procedure_file.is_none());
        assert!(report.trajectory_file.is_some());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("Procedure promotion blocked")));
    }

    #[test]
    fn test_restore_followed_by_read_only_run_has_no_false_proof_or_promotion() {
        let (temp, base_ctx) = create_temp_crate();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let checkpoint_store = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        let write_ctx = base_ctx
            .clone()
            .with_workspace_checkpoint_store(checkpoint_store.clone());
        let options = RuntimeOptions::default();

        let mut first_agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                write_lib_call("write", "pub fn answer() -> u32 {\n    77\n}\n"),
                cargo_check_call("verify"),
                assistant_message("verified update"),
            ])),
            default_tools().into_inner(),
            options.clone(),
        );
        let first_result = first_agent
            .run(&write_ctx, "update src/lib.rs and verify")
            .unwrap();
        assert!(first_result.contains("verified update"));
        assert!(checkpoint_store
            .latest_status()
            .unwrap()
            .expect("checkpoint should exist")
            .captured_paths
            .iter()
            .any(|path| path == "src/lib.rs"));

        let restore_report = checkpoint_store
            .restore_latest()
            .unwrap()
            .expect("restore should succeed");
        assert!(restore_report
            .restored_files
            .iter()
            .any(|path| path == "src/lib.rs"));
        assert_eq!(
            fs::read_to_string(temp.path().join("src/lib.rs")).unwrap(),
            "pub fn answer() -> u32 {\n    42\n}\n"
        );

        let mut second_agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call("read", "read", serde_json::json!({"path": "src/lib.rs"})),
                assistant_message("inspection complete"),
            ])),
            default_tools().into_inner(),
            options.clone(),
        );
        let second_result = second_agent
            .run(&base_ctx, "inspect the restored workspace")
            .unwrap();
        assert!(second_result.contains("inspection complete"));
        assert!(!second_result.contains("### Files Changed"));
        assert!(!second_result.contains("### Verification"));

        let second_task_result = second_agent
            .last_task_result()
            .cloned()
            .expect("expected a structured task result");
        assert!(second_task_result.files_changed().is_empty());
        assert!(second_task_result.verification_commands().is_empty());

        let report = promote_verified_task(
            &memory,
            &base_ctx,
            &options,
            "inspect the restored workspace",
            second_agent.task_mode(),
            &second_task_result,
            &Plan::new(),
            second_agent.durable_memory_written_this_run(),
            &[],
        )
        .unwrap();

        assert_eq!(report, TaskPromotionReport::default());
        assert!(!temp.path().join(MEMORY_LESSONS_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).exists());
        assert!(!temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).exists());
    }

    // ── Observation integration tests ──

    #[test]
    fn test_ensure_layout_creates_observations_dir() {
        let temp = TempDir::new().unwrap();
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        memory.ensure_layout().unwrap();
        assert!(temp.path().join(MEMORY_OBSERVATIONS_RELATIVE_DIR).is_dir());
    }

    #[test]
    fn test_observation_growth_does_not_increase_prompt_size() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | tags: runtime | note: agent lifecycle\n",
        );
        write_topic(
            temp.path(),
            "architecture.md",
            "# Architecture\nruntime session model details",
        );

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());

        // Baseline: no observations
        let baseline_prompt = memory
            .build_prompt("inspect the runtime architecture", None)
            .unwrap();
        let baseline_bytes = baseline_prompt.stats.total_prompt_bytes;

        // Write 100 observations
        let obs_dir = temp.path().join(MEMORY_OBSERVATIONS_RELATIVE_DIR);
        std::fs::create_dir_all(&obs_dir).unwrap();
        for i in 0..100 {
            let record = observation::ObservationRecord {
                id: format!("obs-{:010}-task-{i}", 1000000 + i),
                timestamp_unix_secs: 1000000 + i as i64,
                task_intent: format!("some unrelated task number {i}"),
                source_kind: observation::ObservationSourceKind::Lesson,
                trust_class: observation::ObservationTrustClass::Trusted,
                summary: format!("[lesson] some unrelated task number {i}"),
                artifact_links: observation::ObservationArtifactLinks {
                    lesson_file: Some(format!(".topagent/lessons/{i}-lesson.md")),
                    procedure_file: None,
                    trajectory_file: None,
                    superseded_procedure_file: None,
                },
                changed_files: vec![format!("unrelated/file_{i}.rs")],
                verification_command: None,
            };
            let json = serde_json::to_string_pretty(&record).unwrap();
            std::fs::write(obs_dir.join(format!("{}.json", record.id)), json).unwrap();
        }

        // After: prompt size unchanged (observations don't add prompt content)
        let after_prompt = memory
            .build_prompt("inspect the runtime architecture", None)
            .unwrap();
        assert_eq!(
            after_prompt.stats.total_prompt_bytes, baseline_bytes,
            "100 observations must not increase prompt bytes"
        );
    }

    #[test]
    fn test_observation_boosting_promotes_relevant_artifacts() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: approval-flow | file: lessons/approval-fix.md | status: verified | tags: approval, mailbox | note: approval flow hardening\n- topic: unrelated-topic | file: topics/unrelated.md | status: verified | tags: other | note: something else entirely\n",
        );
        write_lesson(
            temp.path(),
            "approval-fix.md",
            "# Lesson: Approval Fix\n## What Changed\nHardened the approval mailbox.\n## What Learned\nAlways rerun verification.\n",
        );
        write_topic(
            temp.path(),
            "unrelated.md",
            "# Unrelated Topic\nSome other content.\n",
        );

        // Write an observation that links to the approval lesson
        let obs_dir = temp.path().join(MEMORY_OBSERVATIONS_RELATIVE_DIR);
        std::fs::create_dir_all(&obs_dir).unwrap();
        let record = observation::ObservationRecord {
            id: "obs-1-approval-hardening".to_string(),
            timestamp_unix_secs: observation::tests::current_timestamp(),
            task_intent: "harden the approval mailbox".to_string(),
            source_kind: observation::ObservationSourceKind::Lesson,
            trust_class: observation::ObservationTrustClass::Trusted,
            summary: "[lesson] harden the approval mailbox".to_string(),
            artifact_links: observation::ObservationArtifactLinks {
                lesson_file: Some(".topagent/lessons/approval-fix.md".to_string()),
                procedure_file: None,
                trajectory_file: None,
                superseded_procedure_file: None,
            },
            changed_files: vec!["src/approval.rs".to_string()],
            verification_command: Some("cargo test".to_string()),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(obs_dir.join("obs-1-approval-hardening.json"), json).unwrap();

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt("harden the approval mailbox", None)
            .unwrap();

        assert!(prompt.stats.observation_hints_used > 0);
        assert!(prompt.stats.loaded_items.contains(&"approval-flow".to_string()));
    }

    #[test]
    fn test_trajectories_stay_off_prompt_even_with_observation_links() {
        let temp = TempDir::new().unwrap();
        write_memory_index(temp.path(), "# TopAgent Memory Index\n\n");

        // Write an observation linking to a trajectory
        let obs_dir = temp.path().join(MEMORY_OBSERVATIONS_RELATIVE_DIR);
        std::fs::create_dir_all(&obs_dir).unwrap();
        let record = observation::ObservationRecord {
            id: "obs-1-with-trajectory".to_string(),
            timestamp_unix_secs: observation::tests::current_timestamp(),
            task_intent: "fix parsing bug".to_string(),
            source_kind: observation::ObservationSourceKind::Full,
            trust_class: observation::ObservationTrustClass::Trusted,
            summary: "[full] fix parsing bug".to_string(),
            artifact_links: observation::ObservationArtifactLinks {
                lesson_file: None,
                procedure_file: None,
                trajectory_file: Some(
                    ".topagent/trajectories/trj-123-fix-parsing.json".to_string(),
                ),
                superseded_procedure_file: None,
            },
            changed_files: vec!["src/parser.rs".to_string()],
            verification_command: Some("cargo test".to_string()),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(obs_dir.join("obs-1-with-trajectory.json"), json).unwrap();

        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prompt = memory
            .build_prompt("fix parsing bug", None)
            .unwrap();

        // Trajectory content must not appear in the prompt
        if let Some(ref rendered) = prompt.prompt {
            assert!(
                !rendered.contains("trj-123-fix-parsing"),
                "trajectory file must not appear in prompt text"
            );
        }
    }

    #[test]
    fn test_low_trust_observations_do_not_boost_artifacts() {
        let temp = TempDir::new().unwrap();
        write_memory_index(
            temp.path(),
            "# TopAgent Memory Index\n\n- topic: security | file: topics/security.md | status: verified | tags: secret | note: security notes\n",
        );
        write_topic(
            temp.path(),
            "security.md",
            "# Security\nsecret handling details",
        );

        let obs_dir = temp.path().join(MEMORY_OBSERVATIONS_RELATIVE_DIR);
        std::fs::create_dir_all(&obs_dir).unwrap();
        let record = observation::ObservationRecord {
            id: "obs-1-low-trust".to_string(),
            timestamp_unix_secs: observation::tests::current_timestamp(),
            task_intent: "audit secret handling".to_string(),
            source_kind: observation::ObservationSourceKind::Lesson,
            trust_class: observation::ObservationTrustClass::LowTrustPresent,
            summary: "[lesson] audit secret handling".to_string(),
            artifact_links: observation::ObservationArtifactLinks {
                lesson_file: Some(".topagent/topics/security.md".to_string()),
                procedure_file: None,
                trajectory_file: None,
                superseded_procedure_file: None,
            },
            changed_files: vec!["src/secrets.rs".to_string()],
            verification_command: None,
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(obs_dir.join("obs-1-low-trust.json"), json).unwrap();

        let retrieval = observation::progressive_retrieve(&obs_dir, "audit secret handling", 8, 4)
            .unwrap();

        // Low-trust observation is retrieved but its artifacts are not in boost paths
        assert_eq!(retrieval.candidates.len(), 1);
        assert!(retrieval.artifact_paths.is_empty());
        assert!(retrieval.provenance_notes[0].contains("low-trust"));
    }
}

use crate::behavior::{CompactionPolicy, RunStateSnapshot};
use crate::message::{Content, Message};
use crate::session::Session;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionLevel {
    Micro,
    Auto,
    FullRebuild,
}

impl CompactionLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Micro => "Micro",
            Self::Auto => "Auto",
            Self::FullRebuild => "Full Rebuild",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    pub level: CompactionLevel,
    pub before_messages: usize,
    pub after_messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionRuntimeState {
    pub consecutive_auto_failures: usize,
    pub auto_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    InvalidPolicy(&'static str),
}

pub struct TranscriptCompactor<'a> {
    policy: &'a CompactionPolicy,
}

impl<'a> TranscriptCompactor<'a> {
    pub fn new(policy: &'a CompactionPolicy) -> Self {
        Self { policy }
    }

    pub fn micro_compact(
        &self,
        session: &mut Session,
        run_state: &RunStateSnapshot,
    ) -> Option<CompactionOutcome> {
        let messages = session.raw_messages();
        let before = messages.len();
        if before < self.policy.micro_trigger_messages {
            return None;
        }

        let keep_recent = keep_recent_message_count(self.policy);
        let split_at = messages.len().saturating_sub(keep_recent);
        let (older, recent) = messages.split_at(split_at);
        let reduced = summarize_older_tool_history(
            older,
            run_state,
            self.policy.max_compacted_trace_lines,
            CompactionLevel::Micro,
        );
        if !reduced.compacted_any_tool_messages() {
            return None;
        }

        let mut compacted = reduced.kept_messages;
        compacted.push(Message::system(build_summary_message(
            CompactionLevel::Micro,
            run_state,
            reduced.tool_trace_lines,
            reduced.compacted_text_messages,
        )));
        compacted.extend_from_slice(recent);
        session.replace_messages(compacted);

        Some(CompactionOutcome {
            level: CompactionLevel::Micro,
            before_messages: before,
            after_messages: session.message_count(),
        })
    }

    pub fn auto_compact(
        &self,
        session: &mut Session,
        run_state: &RunStateSnapshot,
    ) -> Result<Option<CompactionOutcome>, CompactionError> {
        validate_policy(self.policy)?;

        let messages = session.raw_messages();
        let before = messages.len();
        if before < self.policy.max_messages_before_truncation {
            return Ok(None);
        }

        let keep_recent = keep_recent_message_count(self.policy);
        let split_at = messages.len().saturating_sub(keep_recent);
        let (older, recent) = messages.split_at(split_at);
        let reduced = summarize_transcript(
            older,
            run_state,
            self.policy.max_compacted_trace_lines,
            CompactionLevel::Auto,
        );

        let mut auto_messages = vec![Message::system(build_summary_message(
            CompactionLevel::Auto,
            run_state,
            reduced.tool_trace_lines.clone(),
            reduced.compacted_text_messages,
        ))];
        auto_messages.extend_from_slice(recent);
        session.replace_messages(auto_messages);

        if session.message_count() <= self.policy.max_messages_before_truncation {
            return Ok(Some(CompactionOutcome {
                level: CompactionLevel::Auto,
                before_messages: before,
                after_messages: session.message_count(),
            }));
        }

        let messages = session.raw_messages();
        let rebuild_keep_recent = full_rebuild_recent_message_count(self.policy);
        let split_at = messages.len().saturating_sub(rebuild_keep_recent);
        let (older, recent) = messages.split_at(split_at);
        let reduced = summarize_transcript(
            older,
            run_state,
            self.policy.max_compacted_trace_lines,
            CompactionLevel::FullRebuild,
        );
        let mut rebuilt = vec![Message::system(build_summary_message(
            CompactionLevel::FullRebuild,
            run_state,
            reduced.tool_trace_lines,
            reduced.compacted_text_messages,
        ))];
        rebuilt.extend_from_slice(recent);
        session.replace_messages(rebuilt);

        Ok(Some(CompactionOutcome {
            level: CompactionLevel::FullRebuild,
            before_messages: before,
            after_messages: session.message_count(),
        }))
    }
}

#[derive(Default)]
struct SummaryReduction {
    kept_messages: Vec<Message>,
    tool_trace_lines: Vec<String>,
    compacted_text_messages: usize,
    compacted_tool_messages: usize,
}

impl SummaryReduction {
    fn compacted_any_tool_messages(&self) -> bool {
        self.compacted_tool_messages > 0
    }
}

fn validate_policy(policy: &CompactionPolicy) -> Result<(), CompactionError> {
    if policy.keep_recent_divisor == 0 {
        return Err(CompactionError::InvalidPolicy(
            "keep_recent_divisor must be greater than zero",
        ));
    }
    if policy.max_compacted_trace_lines == 0 {
        return Err(CompactionError::InvalidPolicy(
            "max_compacted_trace_lines must be greater than zero",
        ));
    }
    Ok(())
}

fn summarize_older_tool_history(
    messages: &[Message],
    run_state: &RunStateSnapshot,
    max_trace_lines: usize,
    level: CompactionLevel,
) -> SummaryReduction {
    let mut reduction = SummaryReduction::default();
    let mut traces = Vec::new();
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        if let Some(text) = message.as_text() {
            if is_compaction_notice(text) {
                index += 1;
                continue;
            }
        }

        match &message.content {
            Content::ToolRequest { id, name, args } => {
                let tool_result = messages
                    .get(index + 1)
                    .and_then(|next| match &next.content {
                        Content::ToolResult {
                            id: result_id,
                            result,
                        } if result_id == id => Some(result.as_str()),
                        _ => None,
                    });
                traces.push(ToolTrace::from_request(name, args, tool_result));
                reduction.compacted_tool_messages += 1;
                index += 1;
                if tool_result.is_some() {
                    reduction.compacted_tool_messages += 1;
                    index += 1;
                }
            }
            Content::ToolResult { .. } => {
                reduction.compacted_tool_messages += 1;
                index += 1;
            }
            Content::Text { .. } => {
                reduction.kept_messages.push(message.clone());
                index += 1;
            }
        }
    }

    if reduction.compacted_any_tool_messages() {
        reduction.tool_trace_lines = dedupe_trace_lines(traces, max_trace_lines);
        if reduction.tool_trace_lines.is_empty() {
            reduction.tool_trace_lines.push(format!(
                "{} tool-heavy transcript was compacted. Canonical runtime state stays in the refreshed system prompt.",
                level.label()
            ));
        }
    } else if run_state.memory_context_loaded {
        reduction.kept_messages.push(Message::system(
            "Workspace memory remains preserved in the refreshed system prompt.".to_string(),
        ));
    }

    reduction
}

fn summarize_transcript(
    messages: &[Message],
    _run_state: &RunStateSnapshot,
    max_trace_lines: usize,
    _level: CompactionLevel,
) -> SummaryReduction {
    let mut reduction = SummaryReduction::default();
    let mut traces = Vec::new();
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        if let Some(text) = message.as_text() {
            if is_compaction_notice(text) {
                index += 1;
                continue;
            }
        }

        match &message.content {
            Content::ToolRequest { id, name, args } => {
                let tool_result = messages
                    .get(index + 1)
                    .and_then(|next| match &next.content {
                        Content::ToolResult {
                            id: result_id,
                            result,
                        } if result_id == id => Some(result.as_str()),
                        _ => None,
                    });
                traces.push(ToolTrace::from_request(name, args, tool_result));
                reduction.compacted_tool_messages += 1;
                index += 1;
                if tool_result.is_some() {
                    reduction.compacted_tool_messages += 1;
                    index += 1;
                }
            }
            Content::ToolResult { .. } => {
                reduction.compacted_tool_messages += 1;
                index += 1;
            }
            Content::Text { .. } => {
                reduction.compacted_text_messages += 1;
                index += 1;
            }
        }
    }

    reduction.tool_trace_lines = dedupe_trace_lines(traces, max_trace_lines);
    reduction
}

fn dedupe_trace_lines(traces: Vec<ToolTrace>, max_trace_lines: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for trace in traces.into_iter().rev() {
        if seen.insert(trace.dedupe_key) {
            deduped.push(trace.line);
        }
    }

    deduped.reverse();
    let start = deduped.len().saturating_sub(max_trace_lines);
    deduped.into_iter().skip(start).collect()
}

fn build_summary_message(
    level: CompactionLevel,
    run_state: &RunStateSnapshot,
    tool_trace_lines: Vec<String>,
    compacted_text_messages: usize,
) -> String {
    let mut summary = format!(
        "[{} compaction summary]\nOlder transcript was compacted locally.\n\
Preserved via canonical runtime artifacts and the refreshed system prompt: current objective, current plan, blockers, approvals, active files, workspace memory, proof-of-work anchors, and behavior contract policy.\n\
Current plan remains preserved separately.\n",
        level.label()
    );

    if let Some(objective) = &run_state.objective {
        summary.push_str(&format!(
            "- Objective: {}\n",
            truncate_inline(objective, 180)
        ));
    }

    if run_state.memory_context_loaded {
        summary
            .push_str("- Workspace memory briefing: preserved in the refreshed system prompt.\n");
    }

    if run_state.blockers.is_empty() {
        summary.push_str("- Blockers: none.\n");
    } else {
        summary.push_str("- Blockers:\n");
        for blocker in &run_state.blockers {
            summary.push_str(&format!("  - {}\n", truncate_inline(blocker, 160)));
        }
    }

    if !run_state.pending_approvals.is_empty() {
        summary.push_str("- Pending approvals:\n");
        for approval in &run_state.pending_approvals {
            summary.push_str(&format!("  - {}\n", truncate_inline(approval, 160)));
        }
    }

    if !run_state.recent_approval_decisions.is_empty() {
        summary.push_str("- Recent approval decisions:\n");
        for decision in &run_state.recent_approval_decisions {
            summary.push_str(&format!("  - {}\n", truncate_inline(decision, 160)));
        }
    }

    if !run_state.active_files.is_empty() {
        summary.push_str(&format!(
            "- Active files: {}\n",
            truncate_inline(&run_state.active_files.join(", "), 180)
        ));
    }

    if !run_state.proof_of_work_anchors.is_empty() {
        summary.push_str("- Proof-of-work anchors:\n");
        for anchor in &run_state.proof_of_work_anchors {
            summary.push_str(&format!("  - {}\n", truncate_inline(anchor, 160)));
        }
    }

    if compacted_text_messages > 0 {
        summary.push_str(&format!(
            "- Older conversational messages compacted: {}.\n",
            compacted_text_messages
        ));
    }

    if !tool_trace_lines.is_empty() {
        summary.push_str("- Compacted activity traces:\n");
        for line in tool_trace_lines {
            summary.push_str(&format!("  - {line}\n"));
        }
    }

    summary
}

fn keep_recent_message_count(policy: &CompactionPolicy) -> usize {
    std::cmp::max(
        1,
        policy.max_messages_before_truncation / policy.keep_recent_divisor,
    )
}

fn full_rebuild_recent_message_count(policy: &CompactionPolicy) -> usize {
    std::cmp::max(
        8,
        policy.max_messages_before_truncation / (policy.keep_recent_divisor * 4),
    )
}

fn is_compaction_notice(text: &str) -> bool {
    text.starts_with("[Previous ")
        || text.starts_with("[Micro compaction summary]")
        || text.starts_with("[Auto compaction summary]")
        || text.starts_with("[Full Rebuild compaction summary]")
}

#[derive(Debug)]
struct ToolTrace {
    dedupe_key: String,
    line: String,
}

impl ToolTrace {
    fn from_request(name: &str, args: &serde_json::Value, result: Option<&str>) -> Self {
        match name {
            "read" => {
                let path = args
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<unknown>");
                Self {
                    dedupe_key: format!("read:{path}"),
                    line: format!(
                        "read `{}` -> file excerpt elided; re-run read if exact contents are needed",
                        truncate_inline(path, 100)
                    ),
                }
            }
            "bash" => {
                let command = args
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<unknown>");
                let exit_code = result.and_then(extract_exit_code);
                let outcome = exit_code
                    .map(|code| format!("exit {code}"))
                    .unwrap_or_else(|| "output elided".to_string());
                Self {
                    dedupe_key: format!("bash:{command}"),
                    line: format!("bash `{}` -> {}", truncate_inline(command, 100), outcome),
                }
            }
            "update_plan" => Self {
                dedupe_key: "update_plan".to_string(),
                line: "update_plan -> authoritative plan preserved separately".to_string(),
            },
            "write" | "edit" => {
                let path = args
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("<unknown>");
                Self {
                    dedupe_key: format!("{name}:{path}"),
                    line: format!(
                        "{name} `{}` -> mutation recorded; file state now lives in the workspace",
                        truncate_inline(path, 100)
                    ),
                }
            }
            "save_plan" | "save_lesson" | "manage_operator_preference" => Self {
                dedupe_key: name.to_string(),
                line: format!("{name} -> durable memory write completed"),
            },
            other => {
                let compact_args =
                    truncate_inline(&serde_json::to_string(args).unwrap_or_default(), 100);
                Self {
                    dedupe_key: format!("{other}:{compact_args}"),
                    line: format!("{other} {compact_args} -> output elided"),
                }
            }
        }
    }
}

fn extract_exit_code(result: &str) -> Option<i32> {
    let prefix = "\nExit code: ";
    let pos = result.find(prefix)?;
    let after_prefix = &result[pos + prefix.len()..];
    after_prefix
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>()
        .parse()
        .ok()
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut truncated = String::new();

    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return text.to_string();
        };
        truncated.push(ch);
    }

    if chars.next().is_some() {
        truncated.push_str("...");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> CompactionPolicy {
        CompactionPolicy {
            micro_trigger_messages: 4,
            max_messages_before_truncation: 6,
            keep_recent_divisor: 2,
            max_compacted_trace_lines: 4,
            max_recent_approval_decisions: 3,
            max_recent_proof_of_work_anchors: 4,
            max_failed_auto_compactions: 2,
            refresh_system_prompt_each_turn: true,
            preserved_sections: &["current objective", "current plan"],
        }
    }

    fn snapshot() -> RunStateSnapshot {
        RunStateSnapshot {
            objective: Some("Fix the parser without losing approval state".to_string()),
            blockers: vec!["Approval denied for deleting the generated tool".to_string()],
            pending_approvals: vec!["apr-3 [pending] git commit: release".to_string()],
            recent_approval_decisions: vec!["apr-2 [denied] delete generated tool".to_string()],
            active_files: vec!["src/lib.rs".to_string(), "Cargo.toml".to_string()],
            proof_of_work_anchors: vec!["verification: cargo test --lib (exit 0)".to_string()],
            trust_notes: vec![
                "Low-trust content is active in this run: prior transcript.".to_string()
            ],
            memory_context_loaded: true,
        }
    }

    #[test]
    fn test_micro_compaction_dedupes_old_read_output_and_preserves_recent_tail() {
        let mut session = Session::new();
        session.add_message(Message::user("start"));
        session.add_message(Message::tool_request(
            "read-1",
            "read",
            serde_json::json!({"path": "src/lib.rs"}),
        ));
        session.add_message(Message::tool_result("read-1", "very long excerpt 1"));
        session.add_message(Message::tool_request(
            "read-2",
            "read",
            serde_json::json!({"path": "src/lib.rs"}),
        ));
        session.add_message(Message::tool_result("read-2", "very long excerpt 2"));
        session.add_message(Message::assistant("recent assistant"));
        session.add_message(Message::user("recent user"));

        let policy = policy();
        let compactor = TranscriptCompactor::new(&policy);
        let outcome = compactor
            .micro_compact(&mut session, &snapshot())
            .expect("micro compaction should trigger");

        assert_eq!(outcome.level, CompactionLevel::Micro);
        let messages = session.raw_messages();
        let summary = messages
            .iter()
            .find_map(|message| {
                message
                    .as_text()
                    .filter(|text| text.starts_with("[Micro compaction summary]"))
            })
            .expect("summary message should exist");
        assert!(summary.contains("Fix the parser"));
        assert!(summary.contains("apr-3 [pending] git commit: release"));
        assert_eq!(summary.matches("read `src/lib.rs`").count(), 1);
        assert_eq!(
            messages.last().and_then(Message::as_text),
            Some("recent user")
        );
    }

    #[test]
    fn test_auto_compaction_rebuilds_from_artifacts_and_preserves_recent_tail() {
        let mut session = Session::new();
        for idx in 0..8 {
            session.add_message(Message::user(format!("older user {idx}")));
        }
        session.add_message(Message::assistant("recent assistant"));
        session.add_message(Message::user("recent user"));

        let policy = policy();
        let compactor = TranscriptCompactor::new(&policy);
        let outcome = compactor
            .auto_compact(&mut session, &snapshot())
            .expect("policy should be valid")
            .expect("auto compaction should trigger");

        assert_eq!(outcome.level, CompactionLevel::Auto);
        let messages = session.raw_messages();
        let summary = messages[0].as_text().expect("summary text");
        assert!(summary.starts_with("[Auto compaction summary]"));
        assert!(summary.contains("current plan"));
        assert!(summary.contains("verification: cargo test --lib (exit 0)"));
        assert_eq!(
            messages.last().and_then(Message::as_text),
            Some("recent user")
        );
    }

    #[test]
    fn test_auto_compaction_escalates_to_full_rebuild_when_recent_tail_is_still_too_large() {
        let mut session = Session::new();
        for idx in 0..10 {
            session.add_message(Message::user(format!("message {idx}")));
        }

        let policy = CompactionPolicy {
            keep_recent_divisor: 1,
            ..policy()
        };
        let compactor = TranscriptCompactor::new(&policy);
        let outcome = compactor
            .auto_compact(&mut session, &snapshot())
            .expect("policy should be valid")
            .expect("auto compaction should trigger");

        assert_eq!(outcome.level, CompactionLevel::FullRebuild);
        let messages = session.raw_messages();
        let summary = messages[0].as_text().expect("summary text");
        assert!(summary.starts_with("[Full Rebuild compaction summary]"));
        assert!(session.message_count() < 10);
    }

    #[test]
    fn test_auto_compaction_rejects_invalid_policy() {
        let mut session = Session::new();
        for idx in 0..6 {
            session.add_message(Message::user(format!("message {idx}")));
        }

        let policy = CompactionPolicy {
            keep_recent_divisor: 0,
            ..policy()
        };
        let compactor = TranscriptCompactor::new(&policy);
        let error = compactor
            .auto_compact(&mut session, &snapshot())
            .expect_err("invalid policy should fail");

        assert_eq!(
            error,
            CompactionError::InvalidPolicy("keep_recent_divisor must be greater than zero")
        );
    }
}

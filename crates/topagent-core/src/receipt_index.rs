use crate::behavior::{BashCommandClass, BehaviorContract};
use crate::task_result::{ToolActionOutcome, ToolActionReceipt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_RECEIPT_ANCHORS: usize = 8;
const MAX_RECEIPT_ANCHOR_CHARS: usize = 160;
const MAX_RECEIPT_SUMMARY_CHARS: usize = 1_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptProofSummary {
    pub failed_or_blocked: Vec<String>,
    pub local_inspection: Vec<String>,
    pub verification: Vec<String>,
    pub low_trust_or_external: Vec<String>,
    pub omitted_count: usize,
}

impl ReceiptProofSummary {
    pub fn has_operator_visible_entries(&self) -> bool {
        !self.failed_or_blocked.is_empty()
            || !self.local_inspection.is_empty()
            || !self.verification.is_empty()
            || !self.low_trust_or_external.is_empty()
    }

    pub fn render_compact(&self) -> String {
        let mut out = String::new();
        push_group(&mut out, "failed/blocked", &self.failed_or_blocked);
        push_group(&mut out, "local inspection", &self.local_inspection);
        push_group(&mut out, "verification receipts", &self.verification);
        push_group(&mut out, "low-trust/external", &self.low_trust_or_external);
        if self.omitted_count > 0 {
            out.push_str(&format!(
                "- omitted: {} additional receipt(s)\n",
                self.omitted_count
            ));
        }
        truncate_chars(out.trim_end().to_string(), MAX_RECEIPT_SUMMARY_CHARS)
    }
}

fn push_group(out: &mut String, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    out.push_str(&format!("- {label}: {}\n", values.join("; ")));
}

pub struct ReceiptIndex<'a> {
    receipts: &'a [ToolActionReceipt],
}

impl<'a> ReceiptIndex<'a> {
    pub fn new(receipts: &'a [ToolActionReceipt]) -> Self {
        Self { receipts }
    }

    pub fn by_skill(&self) -> BTreeMap<&'a str, Vec<&'a ToolActionReceipt>> {
        let mut grouped = BTreeMap::new();
        for receipt in self.receipts {
            grouped
                .entry(receipt.tool_name.as_str())
                .or_insert_with(Vec::new)
                .push(receipt);
        }
        grouped
    }

    pub fn by_phase(&self) -> BTreeMap<&'a str, Vec<&'a ToolActionReceipt>> {
        let mut grouped = BTreeMap::new();
        for receipt in self.receipts {
            grouped
                .entry(receipt.phase.as_str())
                .or_insert_with(Vec::new)
                .push(receipt);
        }
        grouped
    }

    pub fn failed_or_blocked(&self) -> Vec<&'a ToolActionReceipt> {
        self.receipts
            .iter()
            .filter(|receipt| receipt.outcome != ToolActionOutcome::Succeeded)
            .collect()
    }

    pub fn has_successful_local_inspection(&self) -> bool {
        self.receipts
            .iter()
            .any(is_successful_local_inspection_receipt)
    }

    pub fn has_successful_commit_review_inspection(&self) -> bool {
        self.receipts
            .iter()
            .any(is_successful_commit_review_receipt)
    }

    pub fn local_inspection_anchors(&self) -> Vec<String> {
        capped_anchors(
            self.receipts
                .iter()
                .filter(|receipt| is_successful_local_inspection_receipt(receipt))
                .map(receipt_anchor),
        )
    }

    pub fn verification_anchors(&self) -> Vec<String> {
        capped_anchors(
            self.receipts
                .iter()
                .filter(|receipt| {
                    is_successful_receipt(receipt)
                        && receipt.tool_name == "bash"
                        && bash_command_from_receipt(receipt).is_some_and(|command| {
                            BehaviorContract::default().classify_bash_command(command)
                                == BashCommandClass::Verification
                        })
                })
                .map(receipt_anchor),
        )
    }

    pub fn failed_or_blocked_anchors(&self) -> Vec<String> {
        capped_anchors(
            self.receipts
                .iter()
                .filter(|receipt| receipt.outcome != ToolActionOutcome::Succeeded)
                .map(receipt_anchor),
        )
    }

    pub fn low_trust_or_external_anchors(&self) -> Vec<String> {
        capped_anchors(
            self.receipts
                .iter()
                .filter(|receipt| {
                    matches!(receipt.tool_name.as_str(), "web_search" | "external_send")
                })
                .map(receipt_anchor),
        )
    }

    pub fn compact_proof_summary(&self) -> ReceiptProofSummary {
        let failed_or_blocked = self.failed_or_blocked_anchors();
        let local_inspection = self.local_inspection_anchors();
        let verification = self.verification_anchors();
        let low_trust_or_external = self.low_trust_or_external_anchors();
        let visible_count = failed_or_blocked.len()
            + local_inspection.len()
            + verification.len()
            + low_trust_or_external.len();

        ReceiptProofSummary {
            failed_or_blocked,
            local_inspection,
            verification,
            low_trust_or_external,
            omitted_count: self.receipts.len().saturating_sub(visible_count),
        }
    }
}

pub(crate) fn is_successful_receipt(receipt: &ToolActionReceipt) -> bool {
    receipt.admitted && receipt.outcome == ToolActionOutcome::Succeeded
}

pub(crate) fn bash_command_from_receipt(receipt: &ToolActionReceipt) -> Option<&str> {
    receipt.summary.strip_prefix("bash: ").map(str::trim)
}

fn is_successful_local_inspection_receipt(receipt: &ToolActionReceipt) -> bool {
    if !is_successful_receipt(receipt) {
        return false;
    }

    match receipt.tool_name.as_str() {
        "read" | "rg" | "git_diff" | "git_status" | "git_branch" => true,
        "bash" => bash_command_from_receipt(receipt).is_some_and(is_inspection_bash_command),
        _ => false,
    }
}

fn is_successful_commit_review_receipt(receipt: &ToolActionReceipt) -> bool {
    if !is_successful_receipt(receipt) {
        return false;
    }

    match receipt.tool_name.as_str() {
        "git_diff" | "git_status" => true,
        "bash" => bash_command_from_receipt(receipt).is_some_and(is_commit_review_bash_command),
        _ => false,
    }
}

fn is_inspection_bash_command(command: &str) -> bool {
    if BehaviorContract::default().classify_bash_command(command) != BashCommandClass::ResearchSafe
    {
        return false;
    }
    let lower = command.trim().to_ascii_lowercase();
    [
        "pwd",
        "ls",
        "rg",
        "find",
        "grep",
        "cat",
        "head",
        "tail",
        "wc",
        "git status",
        "git diff",
        "git log",
        "git show",
        "git branch",
    ]
    .iter()
    .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} ")))
}

fn is_commit_review_bash_command(command: &str) -> bool {
    let lower = command.trim().to_ascii_lowercase();
    ["git diff", "git log", "git show", "git status"]
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} ")))
}

fn receipt_anchor(receipt: &ToolActionReceipt) -> String {
    let summary = compact_line(&receipt.summary, MAX_RECEIPT_ANCHOR_CHARS);
    redact_secret_like(&format!(
        "#{} {} {}: {}",
        receipt.sequence,
        receipt.tool_name,
        receipt.outcome.label(),
        summary
    ))
}

fn capped_anchors<I>(anchors: I) -> Vec<String>
where
    I: Iterator<Item = String>,
{
    anchors.take(MAX_RECEIPT_ANCHORS).collect()
}

fn compact_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(normalized, max_chars)
}

fn truncate_chars(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn redact_secret_like(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            let key_value_secret = ["token=", "api_key=", "apikey=", "password=", "secret="]
                .iter()
                .any(|needle| lower.contains(needle));
            if key_value_secret
                || lower.starts_with("sk-")
                || lower.contains("topagent_secret")
                || lower.contains("supersecret")
            {
                "[REDACTED_SECRET]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(tool: &str, outcome: ToolActionOutcome, summary: &str) -> ToolActionReceipt {
        ToolActionReceipt::new(
            tool,
            "investigate",
            outcome == ToolActionOutcome::Succeeded,
            outcome,
            summary,
        )
    }

    #[test]
    fn receipt_index_identifies_local_inspection_but_not_web_search() {
        let receipts = vec![
            receipt("web_search", ToolActionOutcome::Succeeded, "web_search"),
            receipt("read", ToolActionOutcome::Succeeded, "read: src/lib.rs"),
        ];
        let index = ReceiptIndex::new(&receipts);

        assert!(index.has_successful_local_inspection());
        assert_eq!(index.local_inspection_anchors().len(), 1);
        assert!(!index.local_inspection_anchors()[0].contains("web_search"));
    }

    #[test]
    fn receipt_index_redacts_secret_like_summary_content() {
        let receipts = vec![receipt(
            "bash",
            ToolActionOutcome::Failed,
            "bash: echo api_key=supersecret",
        )];
        let summary = ReceiptIndex::new(&receipts).compact_proof_summary();
        let rendered = summary.render_compact();

        assert!(rendered.contains("[REDACTED_SECRET]"), "{rendered}");
        assert!(!rendered.contains("supersecret"), "{rendered}");
    }

    #[test]
    fn receipt_index_does_not_clone_huge_outputs_into_summaries() {
        let long = format!("bash: cargo test {}", "x".repeat(10_000));
        let receipts = vec![receipt("bash", ToolActionOutcome::Failed, &long)];
        let rendered = ReceiptIndex::new(&receipts)
            .compact_proof_summary()
            .render_compact();

        assert!(rendered.len() <= MAX_RECEIPT_SUMMARY_CHARS);
        assert!(!rendered.contains(&"x".repeat(1_000)));
    }
}

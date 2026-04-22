use super::{BehaviorContract, CompactionPolicy};
use crate::runtime::RuntimeOptions;

pub(super) fn default_compaction_policy(options: &RuntimeOptions) -> CompactionPolicy {
    CompactionPolicy {
        micro_trigger_messages: std::cmp::max(4, options.max_messages_before_truncation / 2),
        max_messages_before_truncation: options.max_messages_before_truncation,
        keep_recent_divisor: 2,
        max_compacted_trace_lines: 8,
        max_recent_approval_decisions: 3,
        max_recent_proof_of_work_anchors: 4,
        max_failed_auto_compactions: 2,
        refresh_system_prompt_each_turn: true,
        preserved_sections: &[
            "behavior contract",
            "current objective",
            "available tools",
            "project instructions",
            "workspace memory",
            "current plan",
            "blockers",
            "pending approvals",
            "approval decisions",
            "active files",
            "proof-of-work anchors",
        ],
    }
}

impl CompactionPolicy {
    pub(crate) fn keep_recent_message_count(&self) -> usize {
        std::cmp::max(
            1,
            self.max_messages_before_truncation / self.keep_recent_divisor,
        )
    }

    pub(crate) fn full_rebuild_recent_message_count(&self) -> usize {
        std::cmp::max(
            8,
            self.max_messages_before_truncation / (self.keep_recent_divisor * 4),
        )
    }

    pub(crate) fn should_micro_compact(&self, message_count: usize) -> bool {
        message_count >= self.micro_trigger_messages
    }

    pub(crate) fn should_auto_compact(&self, message_count: usize) -> bool {
        message_count >= self.max_messages_before_truncation
    }

    pub(crate) fn build_truncation_notice(&self, dropped_count: usize) -> String {
        format!(
            "[Previous {dropped_count} messages truncated due to context length. \
Preserved via fresh system prompt each turn: {}. Use tools to re-read files if you need earlier context.]",
            self.preserved_sections.join(", ")
        )
    }
}

impl BehaviorContract {
    pub fn keep_recent_message_count(&self) -> usize {
        self.compaction.keep_recent_message_count()
    }

    pub fn full_rebuild_recent_message_count(&self) -> usize {
        self.compaction.full_rebuild_recent_message_count()
    }

    pub fn should_micro_compact(&self, message_count: usize) -> bool {
        self.compaction.should_micro_compact(message_count)
    }

    pub fn should_auto_compact(&self, message_count: usize) -> bool {
        self.compaction.should_auto_compact(message_count)
    }

    pub fn build_truncation_notice(&self, dropped_count: usize) -> String {
        self.compaction.build_truncation_notice(dropped_count)
    }
}

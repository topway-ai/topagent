use topagent_core::channel::telegram::{
    TelegramInlineKeyboardButton, TelegramInlineKeyboardMarkup,
};
use topagent_core::ApprovalEntry;

const APPROVAL_CALLBACK_PREFIX: &str = "approval";

pub(super) fn approval_callback_data(approve: bool, request_id: &str) -> String {
    format!(
        "{APPROVAL_CALLBACK_PREFIX}:{}:{request_id}",
        if approve { "approve" } else { "deny" }
    )
}

pub(super) fn parse_approval_callback_data(data: &str) -> Option<(bool, &str)> {
    let mut parts = data.splitn(3, ':');
    if parts.next()? != APPROVAL_CALLBACK_PREFIX {
        return None;
    }

    let approve = match parts.next()? {
        "approve" => true,
        "deny" => false,
        _ => return None,
    };

    let request_id = parts.next()?.trim();
    if request_id.is_empty() {
        return None;
    }

    Some((approve, request_id))
}

pub(super) fn approval_reply_markup(request_id: &str) -> TelegramInlineKeyboardMarkup {
    TelegramInlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            TelegramInlineKeyboardButton {
                text: "Approve".to_string(),
                callback_data: approval_callback_data(true, request_id),
            },
            TelegramInlineKeyboardButton {
                text: "Deny".to_string(),
                callback_data: approval_callback_data(false, request_id),
            },
        ]],
    }
}

pub(super) fn format_approval_resolution(entry: &ApprovalEntry, approve: bool) -> String {
    format!(
        "Approval {} {}: {}.",
        entry.request.id,
        if approve { "approved" } else { "denied" },
        entry.request.short_summary
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use topagent_core::{
        ApprovalEntry, ApprovalRequest, ApprovalState, ApprovalTriggerKind,
    };
    use std::time::SystemTime;

    fn sample_entry(approved: bool) -> ApprovalEntry {
        ApprovalEntry {
            request: ApprovalRequest {
                id: "apr-1".to_string(),
                action_kind: ApprovalTriggerKind::GitCommit,
                short_summary: "git commit: ship it".to_string(),
                exact_action: "git_commit(message=\"ship it\")".to_string(),
                reason: "commits publish a durable repo milestone".to_string(),
                scope_of_impact: "Creates a new git commit.".to_string(),
                expected_effect: "Staged changes become a commit.".to_string(),
                rollback_hint: Some("Use git revert.".to_string()),
                created_at: SystemTime::UNIX_EPOCH,
            },
            state: if approved { ApprovalState::Approved } else { ApprovalState::Denied },
            resolved_at: Some(SystemTime::UNIX_EPOCH),
            decision_note: None,
        }
    }

    #[test]
    fn test_parse_approval_callback_data_recognizes_buttons() {
        assert_eq!(
            parse_approval_callback_data("approval:approve:apr-7"),
            Some((true, "apr-7"))
        );
        assert_eq!(
            parse_approval_callback_data("approval:deny:apr-9"),
            Some((false, "apr-9"))
        );
        assert_eq!(parse_approval_callback_data("approval:approve:"), None);
        assert_eq!(parse_approval_callback_data("unknown:approve:apr-1"), None);
    }

    #[test]
    fn test_approval_reply_markup_contains_approve_and_deny_buttons() {
        let markup = approval_reply_markup("apr-5");
        assert_eq!(markup.inline_keyboard.len(), 1);
        assert_eq!(markup.inline_keyboard[0].len(), 2);
        assert_eq!(markup.inline_keyboard[0][0].text, "Approve");
        assert_eq!(
            markup.inline_keyboard[0][0].callback_data,
            "approval:approve:apr-5"
        );
        assert_eq!(markup.inline_keyboard[0][1].text, "Deny");
        assert_eq!(
            markup.inline_keyboard[0][1].callback_data,
            "approval:deny:apr-5"
        );
    }

    #[test]
    fn test_format_approval_resolution_includes_short_summary() {
        let entry = sample_entry(true);
        let result = format_approval_resolution(&entry, true);
        assert_eq!(result, "Approval apr-1 approved: git commit: ship it.");

        let entry = sample_entry(false);
        let result = format_approval_resolution(&entry, false);
        assert_eq!(result, "Approval apr-1 denied: git commit: ship it.");
    }
}

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
        "Approval {} {}.",
        entry.request.id,
        if approve { "approved" } else { "denied" }
    )
}

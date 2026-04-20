use topagent_core::channel::telegram::{
    TelegramCallbackQuery, TelegramInlineKeyboardMarkup, TelegramMessage,
};
use topagent_core::{context::ExecutionContext, SecretRegistry, TelegramAdapter};
use tracing::{info, warn};

use crate::telegram::admission::{decide_inbound_admission, InboundAdmission};
use crate::telegram::approval::{format_approval_resolution, parse_approval_callback_data};
use crate::telegram::commands::{
    handle_approve, handle_approvals, handle_deny, handle_help, handle_reset, handle_stop,
};
use crate::telegram::delivery::send_telegram;
use crate::telegram::session::ChatSessionManager;

pub(super) fn route_callback_query(
    adapter: &TelegramAdapter,
    session_manager: &mut ChatSessionManager,
    callback: &TelegramCallbackQuery,
) {
    let Some(message) = &callback.message else {
        let _ = adapter.answer_callback_query(
            &callback.id,
            Some("This approval action is no longer available."),
            false,
        );
        return;
    };

    let chat_id = message.chat.id;
    if message.chat.chat_type != "private" {
        let _ = adapter.answer_callback_query(
            &callback.id,
            Some("This bot supports private chats only."),
            false,
        );
        return;
    }

    let Some(data) = callback.data.as_deref() else {
        let _ = adapter.answer_callback_query(&callback.id, Some("Unsupported action."), false);
        return;
    };

    info!("received callback from chat {}: {}", chat_id, data);

    let Some((approve, id)) = parse_approval_callback_data(data) else {
        let _ = adapter.answer_callback_query(&callback.id, Some("Unsupported action."), false);
        return;
    };

    match session_manager.resolve_approval_callback(chat_id, id, approve) {
        Ok(entry) => {
            let reply = format_approval_resolution(&entry, approve);
            if let Err(err) = adapter.answer_callback_query(&callback.id, Some(&reply), false) {
                warn!("failed to answer callback query: {}", err);
            }
            if let Err(err) = adapter.edit_message_text(
                chat_id,
                message.message_id,
                &reply,
                Some(&TelegramInlineKeyboardMarkup {
                    inline_keyboard: vec![],
                }),
            ) {
                let msg = err.to_string();
                if !msg.contains("message is not modified") {
                    warn!(
                        "failed to update approval message after resolution: {}",
                        err
                    );
                }
            }
        }
        Err(err) => {
            if let Err(e) = adapter.answer_callback_query(&callback.id, Some(&err), true) {
                warn!("failed to answer callback query: {}", e);
            }
        }
    }
}

pub(super) fn route_message(
    adapter: &TelegramAdapter,
    session_manager: &mut ChatSessionManager,
    ctx: &ExecutionContext,
    secrets: &SecretRegistry,
    msg: &TelegramMessage,
    workspace_label: &str,
) {
    let chat_id = msg.chat.id;
    let message_id = msg.message_id;

    let sender_username = msg.from.as_ref().and_then(|u| u.username.as_deref());
    let sender_user_id = msg.from.as_ref().map(|u| u.id);

    match decide_inbound_admission(
        &msg.chat.chat_type,
        sender_user_id,
        sender_username,
        |username| session_manager.check_dm_admission(sender_user_id, username),
    ) {
        InboundAdmission::Accept => {}
        InboundAdmission::AcceptAndBind(user_id) => {
            info!(
                "binding allowed DM user: username matched, binding numeric user ID {}",
                user_id
            );
            session_manager.bind_dm_user_id(user_id);
        }
        InboundAdmission::RejectedNonPrivate => {
            send_telegram(&adapter, chat_id, vec!["This bot currently supports private chats only. Open a private chat with the bot and try again.".into()], None);
            return;
        }
        InboundAdmission::RejectedMissingIdentity => {
            send_telegram(&adapter, chat_id, vec!["Access denied. Cannot bind admission without a sender identity.".into()], None);
            return;
        }
        InboundAdmission::Denied => {
            send_telegram(&adapter, chat_id, vec!["Access denied. This bot is not authorized to accept messages from this chat.".into()], None);
            return;
        }
    }

    let Some(ref text) = msg.text else {
        send_telegram(
            &adapter,
            chat_id,
            vec!["This bot currently supports text messages only.".into()],
            None,
        );
        return;
    };

    let text = text.trim();
    if text.is_empty() {
        return;
    }

    info!("received from chat {}: {}", chat_id, text);

    if text == "/start" || text == "/help" {
        let reply = handle_help(
            workspace_label,
            &session_manager.model_label_for_help(),
            session_manager.tool_authoring_enabled(),
            &session_manager.dm_access_label(),
        );
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    if text == "/stop" {
        let reply = handle_stop(session_manager, chat_id);
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    if text == "/approvals" {
        let reply = handle_approvals(session_manager, chat_id);
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    if let Some(argument) = text.strip_prefix("/approve") {
        let reply = handle_approve(session_manager, chat_id, argument);
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    if let Some(argument) = text.strip_prefix("/deny") {
        let reply = handle_deny(session_manager, chat_id, argument);
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    if text == "/reset" {
        let reply = handle_reset(session_manager, chat_id);
        send_telegram(&adapter, chat_id, vec![reply], None);
        return;
    }

    let response = session_manager.start_message(ctx, adapter, chat_id, text);
    send_telegram(&adapter, chat_id, response, Some(secrets));
    let _ = adapter.acknowledge(chat_id, message_id);
}

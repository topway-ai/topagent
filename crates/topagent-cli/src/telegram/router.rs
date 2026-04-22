use topagent_core::channel::telegram::{
    TelegramCallbackQuery, TelegramInlineKeyboardMarkup, TelegramMessage,
};
use topagent_core::{context::ExecutionContext, SecretRegistry, TelegramAdapter};
use tracing::{info, warn};

use crate::commands::surface::{
    parse_telegram_command, ParsedTelegramCommand, TelegramCommandKind,
};
use crate::telegram::admission::{decide_inbound_admission, InboundAdmission};
use crate::telegram::approval::{format_approval_resolution, parse_approval_callback_data};
use crate::telegram::commands::handle_parsed_command;
use crate::telegram::delivery::{send_telegram, TelegramOutbound};
use crate::telegram::session::ChatSessionManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelegramTextRoute<'a> {
    DeclaredCommand {
        kind: TelegramCommandKind,
        argument: &'a str,
    },
    PlainText(&'a str),
    Empty,
    UnsupportedCommand,
    UnsupportedPayload,
}

pub(super) fn route_callback_query(
    adapter: &TelegramAdapter,
    session_manager: &mut ChatSessionManager,
    callback: &TelegramCallbackQuery,
) {
    route_callback_query_with_outbound(adapter, session_manager, callback);
}

fn route_callback_query_with_outbound<T: TelegramOutbound + ?Sized>(
    outbound: &T,
    session_manager: &mut ChatSessionManager,
    callback: &TelegramCallbackQuery,
) {
    let Some(message) = &callback.message else {
        let _ = outbound.answer_callback_query(
            &callback.id,
            Some("This approval action is no longer available."),
            false,
        );
        return;
    };

    let chat_id = message.chat.id;
    if message.chat.chat_type != "private" {
        let _ = outbound.answer_callback_query(
            &callback.id,
            Some("This bot supports private chats only."),
            false,
        );
        return;
    }

    let Some(data) = callback.data.as_deref() else {
        let _ = outbound.answer_callback_query(&callback.id, Some("Unsupported action."), false);
        return;
    };

    info!("received callback from chat {}: {}", chat_id, data);

    let Some((approve, id)) = parse_approval_callback_data(data) else {
        let _ = outbound.answer_callback_query(&callback.id, Some("Unsupported action."), false);
        return;
    };

    match session_manager.resolve_approval_callback(chat_id, id, approve) {
        Ok(entry) => {
            let reply = format_approval_resolution(&entry, approve);
            if let Err(err) = outbound.answer_callback_query(&callback.id, Some(&reply), false) {
                warn!("failed to answer callback query: {}", err);
            }
            if let Err(err) = outbound.edit_message_text(
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
            if let Err(e) = outbound.answer_callback_query(&callback.id, Some(&err), true) {
                warn!("failed to answer callback query: {}", e);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteMessageOutcome<'a> {
    Handled,
    StartTask {
        chat_id: i64,
        message_id: i64,
        text: &'a str,
    },
}

pub(super) fn route_message(
    adapter: &TelegramAdapter,
    session_manager: &mut ChatSessionManager,
    ctx: &ExecutionContext,
    secrets: &SecretRegistry,
    msg: &TelegramMessage,
    workspace_label: &str,
) {
    let RouteMessageOutcome::StartTask {
        chat_id,
        message_id,
        text,
    } = route_message_until_task_start(adapter, session_manager, msg, workspace_label)
    else {
        return;
    };

    info!("received from chat {}: {}", chat_id, text);

    let response = session_manager.start_message(ctx, adapter, chat_id, text);
    deliver_task_start_response(adapter, secrets, chat_id, message_id, response);
}

fn route_message_until_task_start<'a, T: TelegramOutbound + ?Sized>(
    outbound: &T,
    session_manager: &mut ChatSessionManager,
    msg: &'a TelegramMessage,
    workspace_label: &str,
) -> RouteMessageOutcome<'a> {
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
            send_telegram(outbound, chat_id, vec!["This bot currently supports private chats only. Open a private chat with the bot and try again.".into()], None);
            return RouteMessageOutcome::Handled;
        }
        InboundAdmission::RejectedMissingIdentity => {
            send_telegram(
                outbound,
                chat_id,
                vec!["Access denied. Cannot bind admission without a sender identity.".into()],
                None,
            );
            return RouteMessageOutcome::Handled;
        }
        InboundAdmission::Denied => {
            send_telegram(
                outbound,
                chat_id,
                vec![
                    "Access denied. This bot is not authorized to accept messages from this chat."
                        .into(),
                ],
                None,
            );
            return RouteMessageOutcome::Handled;
        }
    }

    let text = match classify_telegram_text_route(msg.text.as_deref()) {
        TelegramTextRoute::UnsupportedPayload => {
            send_telegram(
                outbound,
                chat_id,
                vec!["This bot currently supports text messages only.".into()],
                None,
            );
            return RouteMessageOutcome::Handled;
        }
        TelegramTextRoute::Empty => return RouteMessageOutcome::Handled,
        TelegramTextRoute::DeclaredCommand { kind, argument } => {
            info!(
                "received command from chat {}: {:?} {}",
                chat_id, kind, argument
            );
            let reply = handle_parsed_command(
                ParsedTelegramCommand { kind, argument },
                session_manager,
                chat_id,
                workspace_label,
            );
            send_telegram(outbound, chat_id, vec![reply], None);
            return RouteMessageOutcome::Handled;
        }
        TelegramTextRoute::UnsupportedCommand => {
            send_telegram(
                outbound,
                chat_id,
                vec!["Unsupported command. Send /help to see available commands.".into()],
                None,
            );
            return RouteMessageOutcome::Handled;
        }
        TelegramTextRoute::PlainText(text) => text,
    };

    RouteMessageOutcome::StartTask {
        chat_id,
        message_id,
        text,
    }
}

fn deliver_task_start_response<T: TelegramOutbound + ?Sized>(
    outbound: &T,
    secrets: &SecretRegistry,
    chat_id: i64,
    message_id: i64,
    response: Vec<String>,
) {
    send_telegram(outbound, chat_id, response, Some(secrets));
    let _ = outbound.acknowledge(chat_id, message_id);
}

fn classify_telegram_text_route(text: Option<&str>) -> TelegramTextRoute<'_> {
    let Some(text) = text else {
        return TelegramTextRoute::UnsupportedPayload;
    };
    let text = text.trim();
    if text.is_empty() {
        return TelegramTextRoute::Empty;
    }

    if let Some(command) = parse_telegram_command(text) {
        return TelegramTextRoute::DeclaredCommand {
            kind: command.kind,
            argument: command.argument,
        };
    }

    if text.starts_with('/') {
        return TelegramTextRoute::UnsupportedCommand;
    }

    TelegramTextRoute::PlainText(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::surface::TELEGRAM_COMMANDS;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use topagent_core::channel::telegram::{
        ChannelError, TelegramChat, TelegramInlineKeyboardMarkup, TelegramUser,
    };
    use topagent_core::{
        ApprovalCheck, ApprovalMailbox, ApprovalMailboxMode, ApprovalRequestDraft,
        ApprovalTriggerKind, CancellationToken, ModelRoute, RuntimeOptions,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SentMessage {
        chat_id: i64,
        text: String,
        has_markup: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CallbackAnswer {
        callback_query_id: String,
        text: Option<String>,
        show_alert: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct EditedMessage {
        chat_id: i64,
        message_id: i64,
        text: String,
        markup: Option<TelegramInlineKeyboardMarkup>,
    }

    #[derive(Debug, Default)]
    struct FakeTelegramOutbound {
        sent: RefCell<Vec<SentMessage>>,
        answers: RefCell<Vec<CallbackAnswer>>,
        edits: RefCell<Vec<EditedMessage>>,
        acks: RefCell<Vec<(i64, i64)>>,
    }

    impl TelegramOutbound for FakeTelegramOutbound {
        fn send_message_to_chat(&self, chat_id: i64, text: &str) -> Result<(), ChannelError> {
            self.sent.borrow_mut().push(SentMessage {
                chat_id,
                text: text.to_string(),
                has_markup: false,
            });
            Ok(())
        }

        fn send_message_to_chat_with_markup(
            &self,
            chat_id: i64,
            text: &str,
            reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        ) -> Result<(), ChannelError> {
            self.sent.borrow_mut().push(SentMessage {
                chat_id,
                text: text.to_string(),
                has_markup: reply_markup.is_some(),
            });
            Ok(())
        }

        fn answer_callback_query(
            &self,
            callback_query_id: &str,
            text: Option<&str>,
            show_alert: bool,
        ) -> Result<(), ChannelError> {
            self.answers.borrow_mut().push(CallbackAnswer {
                callback_query_id: callback_query_id.to_string(),
                text: text.map(ToString::to_string),
                show_alert,
            });
            Ok(())
        }

        fn edit_message_text(
            &self,
            chat_id: i64,
            message_id: i64,
            text: &str,
            reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        ) -> Result<(), ChannelError> {
            self.edits.borrow_mut().push(EditedMessage {
                chat_id,
                message_id,
                text: text.to_string(),
                markup: reply_markup.cloned(),
            });
            Ok(())
        }

        fn acknowledge(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError> {
            self.acks.borrow_mut().push((chat_id, message_id));
            Ok(())
        }
    }

    fn test_manager(
        workspace_root: PathBuf,
        allowed_dm_username: Option<String>,
        bound_dm_user_id: Option<i64>,
    ) -> ChatSessionManager {
        ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace_root,
            topagent_core::SecretRegistry::new(),
            allowed_dm_username,
            bound_dm_user_id,
            None,
        )
    }

    fn telegram_message(
        chat_id: i64,
        chat_type: &str,
        text: Option<&str>,
        sender_user_id: Option<i64>,
        sender_username: Option<&str>,
    ) -> TelegramMessage {
        TelegramMessage {
            message_id: 17,
            chat: TelegramChat {
                id: chat_id,
                chat_type: chat_type.to_string(),
            },
            text: text.map(ToString::to_string),
            from: sender_user_id.map(|id| TelegramUser {
                id,
                is_bot: false,
                first_name: "Operator".to_string(),
                username: sender_username.map(ToString::to_string),
            }),
        }
    }

    fn pending_approval_mailbox() -> ApprovalMailbox {
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let check = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::GitCommit,
                short_summary: "git commit: ship it".to_string(),
                exact_action: "git_commit(message=\"ship it\")".to_string(),
                reason: "commits publish a durable repo milestone".to_string(),
                scope_of_impact: "Creates a new git commit in the workspace repository."
                    .to_string(),
                expected_effect: "Staged changes become a durable repo milestone.".to_string(),
                rollback_hint: Some(
                    "Use git revert or git reset if the commit was mistaken.".to_string(),
                ),
            },
            None,
        );
        assert!(matches!(check, ApprovalCheck::Pending(_)));
        mailbox
    }

    #[test]
    fn test_classify_telegram_text_route_matches_declared_command_surface() {
        for spec in TELEGRAM_COMMANDS {
            let text = if spec.arguments.is_empty() {
                spec.command.to_string()
            } else {
                format!("{} apr-1", spec.command)
            };
            assert_eq!(
                classify_telegram_text_route(Some(&text)),
                TelegramTextRoute::DeclaredCommand {
                    kind: spec.kind,
                    argument: if spec.arguments.is_empty() {
                        ""
                    } else {
                        "apr-1"
                    },
                },
                "declared command must classify through the router: {text}"
            );
        }
    }

    #[test]
    fn test_classify_telegram_text_route_separates_payload_cases() {
        assert_eq!(
            classify_telegram_text_route(None),
            TelegramTextRoute::UnsupportedPayload
        );
        assert_eq!(
            classify_telegram_text_route(Some("  \n ")),
            TelegramTextRoute::Empty
        );
        assert_eq!(
            classify_telegram_text_route(Some("/approveabc")),
            TelegramTextRoute::UnsupportedCommand
        );
        assert_eq!(
            classify_telegram_text_route(Some("/unknown")),
            TelegramTextRoute::UnsupportedCommand
        );
        assert_eq!(
            classify_telegram_text_route(Some("  inspect this repo  ")),
            TelegramTextRoute::PlainText("inspect this repo")
        );
    }

    #[test]
    fn test_route_message_with_fake_outbound_rejects_non_private_chat() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf(), None, None);
        let outbound = FakeTelegramOutbound::default();
        let message = telegram_message(42, "group", Some("/help"), Some(42), Some("operator"));

        let outcome =
            route_message_until_task_start(&outbound, &mut manager, &message, "/workspace");

        assert_eq!(outcome, RouteMessageOutcome::Handled);
        let sent = outbound.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].chat_id, 42);
        assert!(sent[0].text.contains("private chats only"));
        assert!(outbound.acks.borrow().is_empty());
    }

    #[test]
    fn test_route_message_with_fake_outbound_binds_allowed_dm_and_handles_command() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(
            workspace.path().to_path_buf(),
            Some("operator".to_string()),
            None,
        );
        let outbound = FakeTelegramOutbound::default();
        let message =
            telegram_message(42, "private", Some("/help"), Some(424242), Some("Operator"));

        let outcome =
            route_message_until_task_start(&outbound, &mut manager, &message, "/workspace");

        assert_eq!(outcome, RouteMessageOutcome::Handled);
        assert_eq!(manager.bound_dm_user_id, Some(424242));
        let sent = outbound.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].text.contains("TopAgent"));
        assert!(sent[0].text.contains("Commands:"));
        assert!(sent[0]
            .text
            .contains("DM access: restricted to @operator (bound)"));
    }

    #[test]
    fn test_route_message_with_fake_outbound_returns_task_start_for_plain_text() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf(), None, None);
        let outbound = FakeTelegramOutbound::default();
        let message = telegram_message(
            42,
            "private",
            Some("  inspect this workspace  "),
            Some(42),
            Some("operator"),
        );

        let outcome =
            route_message_until_task_start(&outbound, &mut manager, &message, "/workspace");

        assert_eq!(
            outcome,
            RouteMessageOutcome::StartTask {
                chat_id: 42,
                message_id: 17,
                text: "inspect this workspace",
            }
        );
        assert!(outbound.sent.borrow().is_empty());
        assert!(outbound.acks.borrow().is_empty());
    }

    #[test]
    fn test_deliver_task_start_response_with_fake_outbound_sends_and_acknowledges() {
        let outbound = FakeTelegramOutbound::default();
        let mut secrets = SecretRegistry::new();
        secrets.register("telegram-secret");

        deliver_task_start_response(
            &outbound,
            &secrets,
            42,
            17,
            vec!["visible reply with telegram-secret".to_string()],
        );

        let sent = outbound.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].chat_id, 42);
        assert!(!sent[0].text.contains("telegram-secret"));
        assert_eq!(outbound.acks.borrow().as_slice(), &[(42, 17)]);
    }

    #[test]
    fn test_route_callback_with_fake_outbound_answers_and_edits_approval() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf(), None, None);
        let mailbox = pending_approval_mailbox();
        manager.sessions.insert(
            42,
            crate::telegram::session::RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: mailbox.clone(),
                instruction: "make a commit".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );
        let outbound = FakeTelegramOutbound::default();
        let callback = TelegramCallbackQuery {
            id: "callback-1".to_string(),
            message: Some(telegram_message(
                42,
                "private",
                Some("approval request"),
                Some(42),
                Some("operator"),
            )),
            data: Some("approval:approve:apr-1".to_string()),
        };

        route_callback_query_with_outbound(&outbound, &mut manager, &callback);

        let answers = outbound.answers.borrow();
        assert_eq!(answers.len(), 1);
        assert_eq!(
            answers[0],
            CallbackAnswer {
                callback_query_id: "callback-1".to_string(),
                text: Some("Approval apr-1 approved.".to_string()),
                show_alert: false,
            }
        );
        let edits = outbound.edits.borrow();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].chat_id, 42);
        assert_eq!(edits[0].message_id, 17);
        assert_eq!(edits[0].text, "Approval apr-1 approved.");
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            topagent_core::ApprovalState::Approved
        );
    }

    #[test]
    fn test_route_callback_with_fake_outbound_rejects_unsupported_payload() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf(), None, None);
        let outbound = FakeTelegramOutbound::default();
        let callback = TelegramCallbackQuery {
            id: "callback-unsupported".to_string(),
            message: Some(telegram_message(
                42,
                "private",
                Some("approval request"),
                Some(42),
                Some("operator"),
            )),
            data: Some("unknown:approve:apr-1".to_string()),
        };

        route_callback_query_with_outbound(&outbound, &mut manager, &callback);

        assert_eq!(
            outbound.answers.borrow().as_slice(),
            &[CallbackAnswer {
                callback_query_id: "callback-unsupported".to_string(),
                text: Some("Unsupported action.".to_string()),
                show_alert: false,
            }]
        );
        assert!(outbound.edits.borrow().is_empty());
    }
}

use anyhow::Result;
use topagent_core::channel::telegram::TelegramInlineKeyboardMarkup;
use topagent_core::{context::ExecutionContext, TelegramAdapter, POLL_TIMEOUT_SECS};
use tracing::{debug, error, info, warn};

use crate::config::{load_persisted_telegram_defaults, resolve_telegram_mode_config, CliParams};
use crate::operational_paths::managed_service_env_path;
use crate::telegram::admission::{decide_inbound_admission, InboundAdmission};
use crate::telegram::approval::{format_approval_resolution, parse_approval_callback_data};
use crate::telegram::delivery::send_telegram;
use crate::telegram::session::ChatSessionManager;

mod admission;
mod approval;
pub(crate) mod delivery;
pub(crate) mod history;
mod session;

pub(crate) use history::clear_workspace_telegram_history;

pub(crate) fn run_telegram(token: Option<String>, params: CliParams) -> Result<()> {
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let config = resolve_telegram_mode_config(token, params, persisted_defaults)?;
    let api_key = config.effective_api_key()?;
    let token = config.token;
    let workspace = config.workspace;
    let configured_default_model = config.configured_default_model;
    let allowed_dm_username = config.allowed_dm_username;
    let bound_dm_user_id = config.bound_dm_user_id;
    let mut secrets = topagent_core::SecretRegistry::new();
    if let Some(ref openrouter_key) = config.openrouter_api_key {
        secrets.register(openrouter_key);
    }
    if let Some(ref opencode_key) = config.opencode_api_key {
        secrets.register(opencode_key);
    }
    secrets.register(&token);
    let ctx = ExecutionContext::new(workspace).with_secrets(secrets.clone());
    let workspace_label = ctx.workspace_root.display().to_string();
    let options = config.options;
    let route = config.route;
    let adapter = TelegramAdapter::new(&token);

    match adapter.check_webhook() {
        Ok(true) => {
            return Err(anyhow::anyhow!(
                "Telegram webhook is configured. Please remove it before using long polling.\n\
                 Use deleteWebhook to disable the webhook: https://core.telegram.org/bots/api#deletewebhook"
            ));
        }
        Ok(false) => {}
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to check Telegram webhook state: {}. Check the bot token and network access.",
                e
            ));
        }
    }

    let bot_info = adapter.get_me().map_err(|e| {
        anyhow::anyhow!(
            "Failed to validate bot token (getMe failed): {}. \
             Make sure TELEGRAM_BOT_TOKEN is correct.",
            e
        )
    })?;

    info!(
        "starting Telegram mode | model: {} | workspace: {}",
        route.model_id, workspace_label
    );
    info!(
        "bot: @{} (id: {}) | private text chats only | send /start in a private chat",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
    );

    let mut session_manager = ChatSessionManager::new(
        route,
        configured_default_model,
        api_key,
        options,
        ctx.workspace_root.clone(),
        secrets.clone(),
        allowed_dm_username,
        bound_dm_user_id,
        Some(managed_service_env_path().ok()).flatten(),
    );
    let mut offset = 0i64;
    let mut polling_retries = 0usize;

    info!("telegram polling started");

    loop {
        session_manager.collect_finished_tasks();
        match adapter.get_updates(
            Some(offset),
            Some(POLL_TIMEOUT_SECS),
            Some(&["message", "callback_query"]),
        ) {
            Ok(updates) => {
                debug!(
                    "get_updates call succeeded, returned {} updates",
                    updates.len()
                );
                if polling_retries > 0 {
                    info!(
                        "telegram polling recovered after {} retries",
                        polling_retries
                    );
                    session_manager.notify_polling_recovered();
                }
                polling_retries = 0;
                for update in updates {
                    offset = update.update_id + 1;

                    if let Some(callback) = &update.callback_query {
                        let Some(message) = &callback.message else {
                            let _ = adapter.answer_callback_query(
                                &callback.id,
                                Some("This approval action is no longer available."),
                                false,
                            );
                            continue;
                        };

                        let chat_id = message.chat.id;
                        if message.chat.chat_type != "private" {
                            let _ = adapter.answer_callback_query(
                                &callback.id,
                                Some("This bot supports private chats only."),
                                false,
                            );
                            continue;
                        }

                        let Some(data) = callback.data.as_deref() else {
                            let _ = adapter.answer_callback_query(
                                &callback.id,
                                Some("Unsupported action."),
                                false,
                            );
                            continue;
                        };

                        info!("received callback from chat {}: {}", chat_id, data);

                        let Some((approve, id)) = parse_approval_callback_data(data) else {
                            let _ = adapter.answer_callback_query(
                                &callback.id,
                                Some("Unsupported action."),
                                false,
                            );
                            continue;
                        };

                        match session_manager.resolve_approval_callback(chat_id, id, approve) {
                            Ok(entry) => {
                                let reply = format_approval_resolution(&entry, approve);
                                if let Err(err) =
                                    adapter.answer_callback_query(&callback.id, Some(&reply), false)
                                {
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
                                if let Err(e) =
                                    adapter.answer_callback_query(&callback.id, Some(&err), true)
                                {
                                    warn!("failed to answer callback query: {}", e);
                                }
                            }
                        }
                        continue;
                    }

                    let Some(msg) = &update.message else { continue };
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
                            continue;
                        }
                        InboundAdmission::RejectedMissingIdentity => {
                            send_telegram(&adapter, chat_id, vec!["Access denied. Cannot bind admission without a sender identity.".into()], None);
                            continue;
                        }
                        InboundAdmission::Denied => {
                            send_telegram(&adapter, chat_id, vec!["Access denied. This bot is not authorized to accept messages from this chat.".into()], None);
                            continue;
                        }
                    }

                    let Some(text) = msg.text.clone() else {
                        send_telegram(
                            &adapter,
                            chat_id,
                            vec!["This bot currently supports text messages only.".into()],
                            None,
                        );
                        continue;
                    };

                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }

                    debug!("Processing message: chat_id={}, text={:?}", chat_id, text);
                    info!("received from chat {}: {}", chat_id, text);

                    if text == "/start" || text == "/help" {
                        let tool_authoring = if session_manager.tool_authoring_enabled() {
                            "on"
                        } else {
                            "off"
                        };
                        let model_label = session_manager.model_label_for_help();
                        let dm_access = session_manager.dm_access_label();
                        let reply = format!(
                            "TopAgent\n\n\
                             Workspace: {}\n\
                             Model: {}\n\
                             Tool authoring: {}\n\
                             DM access: {}\n\
                             Mode: private text chats only\n\n\
                             Commands:\n\
                             /help - show this message\n\
                             /stop - stop the current task\n\
                             /approvals - list pending approvals for this chat\n\
                             /approve <id> - approve a pending action\n\
                             /deny <id> - deny a pending action\n\
                             /reset - clear this chat's saved transcript\n\n\
                             Approval requests include Approve/Deny buttons; slash commands remain available.\n\n\
                             Send a plain text message to start a task.",
                            workspace_label, model_label, tool_authoring, dm_access
                        );
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    if text == "/stop" {
                        let reply = if session_manager.stop_chat(chat_id) {
                            "Stopping current task...".to_string()
                        } else {
                            "No task is currently running.".to_string()
                        };
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    if text == "/approvals" {
                        let reply = session_manager.pending_approvals_reply(chat_id);
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    if let Some(argument) = text.strip_prefix("/approve") {
                        let reply =
                            session_manager.resolve_approval_command(chat_id, argument, true);
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    if let Some(argument) = text.strip_prefix("/deny") {
                        let reply =
                            session_manager.resolve_approval_command(chat_id, argument, false);
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    if text == "/reset" {
                        let reply = if session_manager.is_task_running(chat_id) {
                            "A task is still running. Send /stop and wait for it to finish before /reset."
                                .to_string()
                        } else {
                            session_manager.reset_chat(chat_id);
                            "Saved chat transcript cleared for this chat. Curated workspace memory was left unchanged."
                                .to_string()
                        };
                        send_telegram(&adapter, chat_id, vec![reply], None);
                        continue;
                    }

                    let response = session_manager.start_message(&ctx, &adapter, chat_id, text);
                    send_telegram(&adapter, chat_id, response, Some(&secrets));
                    let _ = adapter.acknowledge(chat_id, message_id);
                }
            }
            Err(e) => {
                polling_retries += 1;
                session_manager.notify_polling_retry();
                let backoff = std::cmp::min(5 * polling_retries as u64, 30);
                if polling_retries <= 3 {
                    warn!(
                        "telegram polling failed: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                } else {
                    error!(
                        "telegram polling sustained failure: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(backoff));
            }
        }
    }
}

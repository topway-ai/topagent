use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use topagent_core::channel::telegram::{
    TelegramInlineKeyboardButton, TelegramInlineKeyboardMarkup,
};
use topagent_core::{
    Agent, ApprovalEntry, ApprovalMailbox, ApprovalMailboxMode, CancellationToken, Message,
    POLL_TIMEOUT_SECS, ProgressCallback, ProgressUpdate, Role, RuntimeOptions, TelegramAdapter,
    WorkspaceCheckpointStore, context::ExecutionContext, model::ModelRoute,
};
use tracing::{error, info, warn};

use crate::config::*;
use crate::managed_files::write_managed_file;
use crate::memory::{WorkspaceMemory, promote_verified_task};
use crate::progress::LiveProgress;
use crate::run_setup::{
    PreparedRunContext, build_agent, prepare_run_context, prepare_workspace_memory,
};

const TELEGRAM_HISTORY_VERSION: u32 = 1;
const MAX_PERSISTED_TRANSCRIPT_MESSAGES: usize = 100;
const APPROVAL_CALLBACK_PREFIX: &str = "approval";

pub(crate) fn run_telegram(token: Option<String>, params: CliParams) -> Result<()> {
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let config = resolve_telegram_mode_config(token, params.clone(), persisted_defaults.clone())?;
    let model_selection = resolve_runtime_model_selection(params.model, persisted_defaults.model);
    let api_key = config.effective_api_key()?;
    let token = config.token;
    let workspace = config.workspace;
    // Register known secrets for redaction in tool output and final replies.
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
        model_selection.configured_default.model_id,
        api_key,
        options,
        ctx.workspace_root.clone(),
        secrets.clone(),
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
                                    adapter.answer_callback_query(&callback.id, Some(&reply), true)
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

                    if msg.chat.chat_type != "private" {
                        send_telegram(&adapter, chat_id, vec!["This bot currently supports private chats only. Open a private chat with the bot and try again.".into()], None);
                        continue;
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

                    info!("received from chat {}: {}", chat_id, text);

                    if text == "/start" || text == "/help" {
                        let tool_authoring = if session_manager.tool_authoring_enabled() {
                            "on"
                        } else {
                            "off"
                        };
                        let model_label = session_manager.model_label_for_help();
                        let reply = format!(
                            "TopAgent\n\n\
                             Workspace: {}\n\
                             Model: {}\n\
                             Tool authoring: {}\n\
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
                            workspace_label, model_label, tool_authoring
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

fn send_telegram(
    adapter: &TelegramAdapter,
    chat_id: i64,
    chunks: Vec<String>,
    secrets: Option<&topagent_core::SecretRegistry>,
) -> DeliveryReport {
    send_telegram_with_markup(adapter, chat_id, chunks, None, secrets)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DeliveryReport {
    attempted_chunks: usize,
    delivered_chunks: usize,
    first_error: Option<String>,
}

impl DeliveryReport {
    fn fully_delivered(&self) -> bool {
        self.attempted_chunks > 0 && self.delivered_chunks == self.attempted_chunks
    }
}

fn send_telegram_with_markup(
    adapter: &TelegramAdapter,
    chat_id: i64,
    chunks: Vec<String>,
    reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    secrets: Option<&topagent_core::SecretRegistry>,
) -> DeliveryReport {
    let last_index = chunks.len().saturating_sub(1);
    let mut report = DeliveryReport {
        attempted_chunks: chunks.len(),
        ..DeliveryReport::default()
    };

    for (index, chunk) in chunks.into_iter().enumerate() {
        // Last-mile secret redaction before the message reaches Telegram.
        let text = match secrets {
            Some(reg) => reg.redact(&chunk).into_owned(),
            None => chunk,
        };
        let result = if index == last_index {
            adapter.send_message_to_chat_with_markup(chat_id, &text, reply_markup)
        } else {
            adapter.send_message_to_chat(chat_id, &text)
        };
        if let Err(e) = result {
            error!("failed to send message: {}", e);
            if report.first_error.is_none() {
                report.first_error = Some(e.to_string());
            }
        } else {
            report.delivered_chunks += 1;
        }
    }

    report
}

fn approval_callback_data(approve: bool, request_id: &str) -> String {
    format!(
        "{APPROVAL_CALLBACK_PREFIX}:{}:{request_id}",
        if approve { "approve" } else { "deny" }
    )
}

fn parse_approval_callback_data(data: &str) -> Option<(bool, &str)> {
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

fn approval_reply_markup(request_id: &str) -> TelegramInlineKeyboardMarkup {
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

fn format_approval_resolution(entry: &ApprovalEntry, approve: bool) -> String {
    format!(
        "Approval {} {}.",
        entry.request.id,
        if approve { "approved" } else { "denied" }
    )
}

fn build_persisted_transcript(messages: &[Message], final_response: Option<&str>) -> Vec<Message> {
    let mut transcript: Vec<_> = messages
        .iter()
        .filter_map(|message| match (message.role, message.as_text()) {
            (Role::User, Some(text)) => Some(Message::user(text)),
            (Role::Assistant, Some(text)) => Some(Message::assistant(text)),
            _ => None,
        })
        .collect();

    if let Some(final_response) = final_response {
        if let Some(last_assistant) = transcript
            .iter_mut()
            .rev()
            .find(|message| message.role == Role::Assistant)
        {
            *last_assistant = Message::assistant(final_response);
        } else {
            transcript.push(Message::assistant(final_response));
        }
    }

    if transcript.len() > MAX_PERSISTED_TRANSCRIPT_MESSAGES {
        let keep_start = transcript.len() - MAX_PERSISTED_TRANSCRIPT_MESSAGES;
        transcript.drain(..keep_start);
    }

    transcript
}

fn persist_messages_to_store(history_store: &ChatHistoryStore, chat_id: i64, messages: &[Message]) {
    if messages.is_empty() {
        if let Err(err) = history_store.clear(chat_id) {
            warn!(
                "failed to clear empty Telegram history for chat {} from {}: {}",
                chat_id,
                history_store.path_for_chat(chat_id).display(),
                err
            );
        }
        return;
    }

    match history_store.save(chat_id, messages) {
        Ok(path) => {
            info!(
                "saved {} Telegram history messages for chat {} to {}",
                messages.len(),
                chat_id,
                path.display()
            );
        }
        Err(err) => {
            warn!(
                "failed to save Telegram history for chat {} to {}: {}",
                chat_id,
                history_store.path_for_chat(chat_id).display(),
                err
            );
        }
    }
}

fn persist_visible_exchange_to_store(
    history_store: &ChatHistoryStore,
    chat_id: i64,
    user_text: &str,
    assistant_text: Option<&str>,
) {
    let mut messages = match history_store.load(chat_id) {
        Ok(existing) => build_persisted_transcript(&existing, None),
        Err(err) => {
            warn!(
                "failed to load existing Telegram transcript for chat {} from {} before appending visible exchange: {}",
                chat_id,
                history_store.path_for_chat(chat_id).display(),
                err
            );
            Vec::new()
        }
    };

    let user_text = user_text.trim();
    if !user_text.is_empty() {
        messages.push(Message::user(user_text));
    }

    if let Some(assistant_text) = assistant_text
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        messages.push(Message::assistant(assistant_text));
    }

    if messages.len() > MAX_PERSISTED_TRANSCRIPT_MESSAGES {
        let keep_start = messages.len() - MAX_PERSISTED_TRANSCRIPT_MESSAGES;
        messages.drain(..keep_start);
    }

    persist_messages_to_store(history_store, chat_id, &messages);
}

#[cfg(test)]
fn persist_agent_history_to_store(
    history_store: &ChatHistoryStore,
    chat_id: i64,
    agent: &Agent,
    final_response: Option<&str>,
) {
    let mut messages = match history_store.load(chat_id) {
        Ok(existing) => build_persisted_transcript(&existing, None),
        Err(err) => {
            warn!(
                "failed to load existing Telegram transcript for chat {} from {} before saving: {}",
                chat_id,
                history_store.path_for_chat(chat_id).display(),
                err
            );
            Vec::new()
        }
    };
    messages.extend(build_persisted_transcript(
        &agent.conversation_messages(),
        final_response,
    ));
    if messages.len() > MAX_PERSISTED_TRANSCRIPT_MESSAGES {
        let keep_start = messages.len() - MAX_PERSISTED_TRANSCRIPT_MESSAGES;
        messages.drain(..keep_start);
    }
    persist_messages_to_store(history_store, chat_id, &messages);
}

// ── Chat history persistence ──

#[derive(Debug, Clone)]
struct ChatHistoryStore {
    history_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedChatHistory {
    version: u32,
    messages: Vec<topagent_core::Message>,
}

impl ChatHistoryStore {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            history_dir: workspace_root.join(".topagent").join("telegram-history"),
        }
    }

    fn path_for_chat(&self, chat_id: i64) -> PathBuf {
        self.history_dir.join(format!("chat-{chat_id}.json"))
    }

    fn load(&self, chat_id: i64) -> Result<Vec<topagent_core::Message>> {
        let path = self.path_for_chat(chat_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let history: PersistedChatHistory = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if history.version != TELEGRAM_HISTORY_VERSION {
            return Err(anyhow::anyhow!(
                "unsupported Telegram history version {} in {}",
                history.version,
                path.display()
            ));
        }

        Ok(history.messages)
    }

    fn save(&self, chat_id: i64, messages: &[topagent_core::Message]) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.history_dir)
            .with_context(|| format!("failed to create {}", self.history_dir.display()))?;
        let path = self.path_for_chat(chat_id);
        let history = PersistedChatHistory {
            version: TELEGRAM_HISTORY_VERSION,
            messages: messages.to_vec(),
        };
        let contents = serde_json::to_string_pretty(&history)
            .with_context(|| format!("failed to encode {}", path.display()))?;
        write_managed_file(&path, &contents, true)?;
        Ok(path)
    }

    fn clear(&self, chat_id: i64) -> Result<bool> {
        let path = self.path_for_chat(chat_id);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        Ok(true)
    }

    fn clear_all(&self) -> Result<bool> {
        if !self.history_dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&self.history_dir)
            .with_context(|| format!("failed to remove {}", self.history_dir.display()))?;
        Ok(true)
    }
}

pub(crate) fn clear_workspace_telegram_history(workspace_root: &Path) -> Result<bool> {
    ChatHistoryStore::new(workspace_root.to_path_buf()).clear_all()
}

use anyhow::Context;

// ── Session manager ──

pub(crate) struct ChatSessionManager {
    route: ModelRoute,
    configured_default_model: String,
    api_key: String,
    options: RuntimeOptions,
    history_store: ChatHistoryStore,
    memory: WorkspaceMemory,
    secrets: topagent_core::SecretRegistry,
    pub sessions: HashMap<i64, RunningChatTask>,
    completed_tx: mpsc::Sender<i64>,
    completed_rx: mpsc::Receiver<i64>,
}

pub(crate) struct RunningChatTask {
    pub cancel_token: CancellationToken,
    pub progress_callback: Option<ProgressCallback>,
    pub approval_mailbox: ApprovalMailbox,
}

impl ChatSessionManager {
    pub fn new(
        route: ModelRoute,
        configured_default_model: String,
        api_key: String,
        options: RuntimeOptions,
        workspace_root: PathBuf,
        secrets: topagent_core::SecretRegistry,
    ) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        let memory = prepare_workspace_memory(workspace_root.clone());

        Self {
            route,
            configured_default_model,
            api_key,
            options,
            history_store: ChatHistoryStore::new(workspace_root.clone()),
            memory,
            secrets,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
        }
    }

    fn create_agent(&self) -> Agent {
        build_agent(&self.route, &self.api_key, self.options.clone())
    }

    fn tool_authoring_enabled(&self) -> bool {
        self.options.enable_generated_tool_authoring
    }

    fn model_label_for_help(&self) -> String {
        current_model_label_for_help(&self.route, &self.configured_default_model)
    }

    fn load_redacted_transcript(&self, chat_id: i64) -> Vec<Message> {
        match self.history_store.load(chat_id) {
            Ok(messages) => messages
                .into_iter()
                .map(|message| message.redact_secrets(&self.secrets))
                .collect::<Vec<_>>(),
            Err(err) => {
                warn!(
                    "failed to load Telegram transcript for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
                Vec::new()
            }
        }
    }

    fn build_run_context(
        &self,
        ctx: &ExecutionContext,
        chat_id: i64,
        instruction: &str,
    ) -> PreparedRunContext {
        let transcript = self.load_redacted_transcript(chat_id);
        let prepared = prepare_run_context(ctx, &self.memory, instruction, Some(&transcript));
        PreparedRunContext {
            run_ctx: prepared.run_ctx.with_workspace_checkpoint_store(
                WorkspaceCheckpointStore::new(ctx.workspace_root.clone()),
            ),
            loaded_procedure_files: prepared.loaded_procedure_files,
        }
    }

    #[cfg(test)]
    pub fn persist_agent_history(&self, chat_id: i64, agent: &Agent) {
        persist_agent_history_to_store(&self.history_store, chat_id, agent, None);
    }

    fn collect_finished_tasks(&mut self) {
        while let Ok(chat_id) = self.completed_rx.try_recv() {
            self.sessions.remove(&chat_id);
        }
    }

    fn is_task_running(&self, chat_id: i64) -> bool {
        self.sessions.contains_key(&chat_id)
    }

    fn stop_chat(&mut self, chat_id: i64) -> bool {
        let Some(task) = self.sessions.get(&chat_id) else {
            return false;
        };

        task.approval_mailbox
            .expire_pending("task stopped by operator");
        task.cancel_token.cancel();
        if let Some(callback) = &task.progress_callback {
            callback(ProgressUpdate::stopping());
        }
        true
    }

    fn notify_polling_retry(&self) {
        self.broadcast_progress(ProgressUpdate::retrying(
            "Telegram polling failed, retrying connection...",
        ));
    }

    fn notify_polling_recovered(&self) {
        self.broadcast_progress(ProgressUpdate::working(
            "Telegram connection restored. Task still running...",
        ));
    }

    fn broadcast_progress(&self, update: ProgressUpdate) {
        for task in self.sessions.values() {
            if let Some(callback) = &task.progress_callback {
                callback(update.clone());
            }
        }
    }

    pub fn reset_chat(&mut self, chat_id: i64) {
        if let Some(task) = self.sessions.remove(&chat_id) {
            task.approval_mailbox
                .supersede_pending("chat reset before approval was resolved");
        }
        match self.history_store.clear(chat_id) {
            Ok(true) => {
                info!(
                    "cleared Telegram transcript for chat {} from {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display()
                );
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    "failed to clear Telegram transcript for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
            }
        }
    }

    fn pending_approvals(&self, chat_id: i64) -> Vec<ApprovalEntry> {
        self.sessions
            .get(&chat_id)
            .map(|task| task.approval_mailbox.pending())
            .unwrap_or_default()
    }

    fn pending_approvals_reply(&self, chat_id: i64) -> String {
        let approvals = self.pending_approvals(chat_id);
        if approvals.is_empty() {
            return "No pending approvals for this chat.".to_string();
        }

        let mut reply = String::from("Pending approvals:\n");
        for approval in approvals {
            reply.push_str("- ");
            reply.push_str(&approval.request.render_status_line(approval.state));
            reply.push('\n');
        }
        reply.push_str(
            "\nTap the buttons on the approval message, or reply with /approve <id> or /deny <id>.",
        );
        reply
    }

    fn resolve_approval_request(
        &self,
        chat_id: i64,
        id: &str,
        approve: bool,
        note: &str,
    ) -> std::result::Result<ApprovalEntry, String> {
        let Some(task) = self.sessions.get(&chat_id) else {
            return Err("No task is currently running in this chat.".to_string());
        };

        let result = if approve {
            task.approval_mailbox.approve(id, Some(note.to_string()))
        } else {
            task.approval_mailbox.deny(id, Some(note.to_string()))
        };

        match result {
            Ok(entry) => Ok(entry),
            Err(err) => Err(format!("Could not update approval {}: {}", id, err)),
        }
    }

    fn resolve_approval_command(&self, chat_id: i64, argument: &str, approve: bool) -> String {
        let id = argument.trim();
        if id.is_empty() {
            return if approve {
                "Usage: /approve <id>".to_string()
            } else {
                "Usage: /deny <id>".to_string()
            };
        }

        match self.resolve_approval_request(chat_id, id, approve, "resolved from Telegram command")
        {
            Ok(entry) => format_approval_resolution(&entry, approve),
            Err(err) => err,
        }
    }

    fn resolve_approval_callback(
        &self,
        chat_id: i64,
        id: &str,
        approve: bool,
    ) -> std::result::Result<ApprovalEntry, String> {
        self.resolve_approval_request(chat_id, id, approve, "resolved from Telegram button")
    }

    fn start_message(
        &mut self,
        ctx: &ExecutionContext,
        adapter: &TelegramAdapter,
        chat_id: i64,
        text: &str,
    ) -> Vec<String> {
        self.collect_finished_tasks();
        if self.is_task_running(chat_id) {
            return vec![
                "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                    .to_string(),
            ];
        }

        let heartbeat_interval = Duration::from_secs(self.options.progress_heartbeat_secs);
        let mut agent = self.create_agent();

        let cancel_token = CancellationToken::new();
        let approval_mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let approval_adapter = adapter.clone();
        let approval_secrets = self.secrets.clone();
        approval_mailbox.set_notifier(Arc::new(move |request| {
            let mut message = request.render_details();
            message.push_str(&format!(
                "\n\nTap Approve or Deny below, or reply with /approve {} or /deny {}.",
                request.id, request.id
            ));
            let chunks = topagent_core::channel::telegram::chunk_text(&message, 4000);
            let reply_markup = approval_reply_markup(&request.id);
            send_telegram_with_markup(
                &approval_adapter,
                chat_id,
                chunks,
                Some(&reply_markup),
                Some(&approval_secrets),
            );
        }));
        let prepared_run = self.build_run_context(
            &ctx.clone()
                .with_cancel_token(cancel_token.clone())
                .with_approval_mailbox(approval_mailbox.clone()),
            chat_id,
            text,
        );
        let loaded_procedure_files = prepared_run.loaded_procedure_files.clone();
        let run_ctx = prepared_run.run_ctx;
        let progress =
            match LiveProgress::for_telegram(heartbeat_interval, adapter.clone(), chat_id) {
                Ok(progress) => Some(progress),
                Err(err) => {
                    error!("failed to start Telegram live progress: {}", err);
                    None
                }
            };
        let progress_callback = progress.as_ref().map(|progress| progress.callback());
        let worker_progress_callback = progress_callback.clone();
        let completed_tx = self.completed_tx.clone();
        let history_store = self.history_store.clone();
        let memory = self.memory.clone();
        let worker_secrets = self.secrets.clone();
        let adapter = adapter.clone();
        let instruction = text.to_string();
        let distill_options = self.options.clone();

        thread::spawn(move || {
            let has_progress = worker_progress_callback.is_some();
            let mut promotion_notes = Vec::new();
            if let Some(callback) = &worker_progress_callback {
                agent.set_progress_callback(Some(callback.clone()));
            }

            let result = agent.run(&run_ctx, &instruction);
            agent.set_progress_callback(None);
            if let Ok(_response) = &result {
                if let Some(task_result) = agent.last_task_result().cloned() {
                    match agent.plan().lock() {
                        Ok(plan) => match promote_verified_task(
                            &memory,
                            &run_ctx,
                            &distill_options,
                            &instruction,
                            agent.task_mode(),
                            &task_result,
                            &plan.clone(),
                            agent.durable_memory_written_this_run(),
                            &loaded_procedure_files,
                        ) {
                            Ok(report) => {
                                if report.lesson_file.is_some()
                                    || report.procedure_file.is_some()
                                    || report.trajectory_file.is_some()
                                {
                                    info!(
                                        lesson = report.lesson_file.as_deref().unwrap_or(""),
                                        procedure = report.procedure_file.as_deref().unwrap_or(""),
                                        trajectory =
                                            report.trajectory_file.as_deref().unwrap_or(""),
                                        chat_id,
                                        "saved promoted workspace learning artifacts"
                                    );
                                }
                                promotion_notes = report.notes;
                            }
                            Err(err) => {
                                warn!("failed to promote verified Telegram task memory: {}", err)
                            }
                        },
                        Err(err) => {
                            warn!("failed to lock agent plan for Telegram promotion: {}", err)
                        }
                    }
                }
            }
            if let Some(progress) = progress {
                progress.wait();
            }

            match result {
                Ok(mut response) => {
                    if !promotion_notes.is_empty() {
                        response.push_str("\n\n### Trust Notes\n");
                        for note in promotion_notes {
                            response.push_str(&format!("- {}\n", note));
                        }
                    }
                    let max_len = 4000;
                    let chunks = if response.len() <= max_len {
                        vec![response.clone()]
                    } else {
                        topagent_core::channel::telegram::chunk_text(&response, max_len)
                    };
                    let delivery = send_telegram(&adapter, chat_id, chunks, Some(&worker_secrets));
                    persist_visible_exchange_to_store(
                        &history_store,
                        chat_id,
                        &instruction,
                        delivery.fully_delivered().then_some(response.as_str()),
                    );
                }
                Err(topagent_core::Error::Stopped(_)) => {
                    persist_visible_exchange_to_store(&history_store, chat_id, &instruction, None);
                }
                Err(e) => {
                    // When progress is active, the status message already shows the
                    // failure via ProgressUpdate::failed. Don't send a duplicate error.
                    if !has_progress {
                        let error_text = format!("Error: {}", e);
                        let delivery = send_telegram(
                            &adapter,
                            chat_id,
                            vec![error_text.clone()],
                            Some(&worker_secrets),
                        );
                        persist_visible_exchange_to_store(
                            &history_store,
                            chat_id,
                            &instruction,
                            delivery.fully_delivered().then_some(error_text.as_str()),
                        );
                    } else {
                        persist_visible_exchange_to_store(
                            &history_store,
                            chat_id,
                            &instruction,
                            None,
                        );
                    }
                }
            }

            let _ = completed_tx.send(chat_id);
        });

        self.sessions.insert(
            chat_id,
            RunningChatTask {
                cancel_token,
                progress_callback,
                approval_mailbox,
            },
        );
        Vec::new()
    }
}

fn current_model_label_for_help(
    active_route: &ModelRoute,
    configured_default_model: &str,
) -> String {
    let configured_default_model = configured_default_model.trim();
    if configured_default_model.is_empty() || configured_default_model == active_route.model_id {
        active_route.model_id.clone()
    } else {
        format!(
            "{} (override; default {})",
            active_route.model_id, configured_default_model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::procedures::{ProcedureDraft, save_procedure};
    use crate::memory::{MEMORY_PROCEDURES_RELATIVE_DIR, MEMORY_TRAJECTORIES_RELATIVE_DIR};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::{
        ApprovalCheck, ApprovalRequestDraft, ApprovalTriggerKind, BehaviorContract,
        CancellationToken, Message, ModelRoute, ProgressKind, ProgressUpdate,
    };

    fn test_manager(workspace_root: PathBuf) -> ChatSessionManager {
        ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace_root,
            topagent_core::SecretRegistry::new(),
        )
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
    fn test_stop_chat_cancels_running_task_and_emits_stopping_progress() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let cancel_token = CancellationToken::new();
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
            },
        );

        assert!(manager.stop_chat(42));
        assert!(cancel_token.is_cancelled());
        assert!(
            updates
                .lock()
                .unwrap()
                .iter()
                .any(|update| update == &ProgressUpdate::stopping())
        );
    }

    #[test]
    fn test_stop_chat_returns_false_when_idle() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        assert!(!manager.stop_chat(42));
    }

    #[test]
    fn test_help_model_label_shows_override_and_default_when_they_differ() {
        let label = current_model_label_for_help(
            &ModelRoute::openrouter("anthropic/claude-sonnet-4.6"),
            "qwen/qwen3.6-plus:free",
        );

        assert_eq!(
            label,
            "anthropic/claude-sonnet-4.6 (override; default qwen/qwen3.6-plus:free)"
        );
    }

    #[test]
    fn test_help_model_label_falls_back_to_active_route_when_it_matches_default() {
        let label = current_model_label_for_help(
            &ModelRoute::openrouter("anthropic/claude-sonnet-4.6"),
            "anthropic/claude-sonnet-4.6",
        );

        assert_eq!(label, "anthropic/claude-sonnet-4.6");
    }

    #[test]
    fn test_pending_approvals_reply_lists_request_ids() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: pending_approval_mailbox(),
            },
        );

        let reply = manager.pending_approvals_reply(42);
        assert!(reply.contains("Pending approvals"));
        assert!(reply.contains("apr-1"));
        assert!(reply.contains("Tap the buttons"));
        assert!(reply.contains("/approve <id>"));
    }

    #[test]
    fn test_resolve_approval_command_updates_pending_request() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mailbox = pending_approval_mailbox();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: mailbox.clone(),
            },
        );

        let reply = manager.resolve_approval_command(42, "apr-1", true);
        assert!(reply.contains("Approval apr-1 approved"));
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            topagent_core::ApprovalState::Approved
        );
    }

    #[test]
    fn test_resolve_approval_callback_updates_pending_request() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mailbox = pending_approval_mailbox();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: mailbox.clone(),
            },
        );

        let entry = manager
            .resolve_approval_callback(42, "apr-1", false)
            .unwrap();
        assert_eq!(entry.state, topagent_core::ApprovalState::Denied);
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            topagent_core::ApprovalState::Denied
        );
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
    fn test_notify_polling_retry_emits_retrying_progress_to_running_chat() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
            },
        );

        manager.notify_polling_retry();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Retrying
                && update
                    .message
                    .contains("Telegram polling failed, retrying connection")
        }));
    }

    #[test]
    fn test_notify_polling_recovered_emits_working_progress_to_running_chat() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
            },
        );

        manager.notify_polling_recovered();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Working
                && update
                    .message
                    .contains("Telegram connection restored. Task still running")
        }));
    }

    #[test]
    fn test_memory_context_retrieves_targeted_transcript_snippet_instead_of_restoring_whole_history()
     {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4242;
        let original_manager = test_manager(workspace.path().to_path_buf());
        let mut original_agent = original_manager.create_agent();
        original_agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: maple comet."),
            Message::assistant("Stored. I will remember maple comet."),
            Message::user("Also keep cedar echo."),
            Message::assistant("Stored. I will remember cedar echo."),
        ]);
        persist_agent_history_to_store(
            &original_manager.history_store,
            chat_id,
            &original_agent,
            None,
        );

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let prepared_run = restarted_manager.build_run_context(
            &ExecutionContext::new(workspace.path().to_path_buf()),
            chat_id,
            "What was the maple phrase I mentioned earlier?",
        );
        let memory_context = prepared_run.run_ctx.memory_context().unwrap();

        assert!(memory_context.contains("maple comet"));
        assert!(!memory_context.contains("cedar echo"));
        assert!(
            workspace
                .path()
                .join(".topagent")
                .join("telegram-history")
                .join("chat-4242.json")
                .is_file()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(
                workspace
                    .path()
                    .join(".topagent")
                    .join("telegram-history")
                    .join("chat-4242.json"),
            )
            .unwrap()
            .permissions()
            .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn test_telegram_build_run_context_matches_one_shot_context_and_keeps_prompt_targeted() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4243;
        let manager = test_manager(workspace.path().to_path_buf());

        std::fs::create_dir_all(workspace.path().join(".topagent")).unwrap();
        std::fs::write(
            workspace.path().join(".topagent/USER.md"),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
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
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Irrelevant visual polish flow".to_string(),
                when_to_use: "Use for unrelated diagram theming.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
                steps: vec!["Adjust the diagram palette.".to_string()],
                pitfalls: vec!["Do not use for backend repair tasks.".to_string()],
                verification: "cargo test -p topagent-cli".to_string(),
                source_task: Some("visual polish".to_string()),
                source_lesson: None,
                source_trajectory: None,
                supersedes: None,
            },
        )
        .unwrap();
        std::fs::create_dir_all(workspace.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR)).unwrap();
        std::fs::write(
            workspace
                .path()
                .join(MEMORY_TRAJECTORIES_RELATIVE_DIR)
                .join("ignored.json"),
            r#"{"task_intent":"ignored trajectory"}"#,
        )
        .unwrap();
        manager.memory.consolidate_memory_if_needed().unwrap();
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Remember the maple compaction phrase.",
            Some("Stored. Maple compaction phrase recorded."),
        );
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Also remember the cedar orbit token.",
            Some("Stored. Cedar orbit token recorded."),
        );

        let base_ctx = ExecutionContext::new(workspace.path().to_path_buf());
        let telegram_prepared = manager.build_run_context(
            &base_ctx,
            chat_id,
            "what was the maple phrase I mentioned earlier while repairing approval mailbox compaction?",
        );
        let transcript = manager.history_store.load(chat_id).unwrap();
        let one_shot_prepared = prepare_run_context(
            &base_ctx,
            &manager.memory,
            "what was the maple phrase I mentioned earlier while repairing approval mailbox compaction?",
            Some(&transcript),
        );

        assert_eq!(
            telegram_prepared.loaded_procedure_files,
            one_shot_prepared.loaded_procedure_files
        );
        assert_eq!(
            telegram_prepared.run_ctx.memory_context(),
            one_shot_prepared.run_ctx.memory_context()
        );
        assert_eq!(
            telegram_prepared.run_ctx.operator_context(),
            one_shot_prepared.run_ctx.operator_context()
        );
        assert_eq!(
            telegram_prepared.run_ctx.run_trust_context(),
            one_shot_prepared.run_ctx.run_trust_context()
        );

        let memory_context = telegram_prepared
            .run_ctx
            .memory_context()
            .unwrap_or_default();
        assert!(memory_context.contains("Approval mailbox compaction playbook"));
        assert!(memory_context.contains("maple compaction phrase"));
        assert!(!memory_context.contains("cedar orbit token"));
        assert!(!memory_context.contains("ignored trajectory"));
        assert!(
            telegram_prepared.loaded_procedure_files.len()
                <= BehaviorContract::default().memory.max_procedures_to_load
        );
    }

    #[test]
    fn test_history_is_saved_to_disk_as_user_visible_transcript_only() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 777;
        let manager = test_manager(workspace.path().to_path_buf());
        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: cedar echo."),
            Message::tool_request("tool-1", "bash", serde_json::json!({"command": "pwd"})),
            Message::tool_result("tool-1", "/tmp/workspace"),
            Message::assistant("Stored. I will remember cedar echo."),
        ]);

        persist_agent_history_to_store(&manager.history_store, chat_id, &agent, None);

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(
            persisted[0].as_text(),
            Some("Remember this exact phrase: cedar echo.")
        );
        assert_eq!(
            persisted[1].as_text(),
            Some("Stored. I will remember cedar echo.")
        );
    }

    #[test]
    fn test_persist_visible_exchange_stores_only_delivered_messages() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 1717;
        let manager = test_manager(workspace.path().to_path_buf());

        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Fix the config path.",
            Some("Done. Verified with cargo test."),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].as_text(), Some("Fix the config path."));
        assert_eq!(
            persisted[1].as_text(),
            Some("Done. Verified with cargo test.")
        );
    }

    #[test]
    fn test_persist_visible_exchange_skips_undelivered_assistant_reply() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 1818;
        let manager = test_manager(workspace.path().to_path_buf());

        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Fix the config path.",
            Some("Done. Verified with cargo test."),
        );
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Run it again after the restart.",
            None,
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 3);
        assert_eq!(persisted[0].as_text(), Some("Fix the config path."));
        assert_eq!(
            persisted[1].as_text(),
            Some("Done. Verified with cargo test.")
        );
        assert_eq!(
            persisted[2].as_text(),
            Some("Run it again after the restart.")
        );
    }

    #[test]
    fn test_post_restart_persist_keeps_pre_restart_exchange_in_file() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 5150;
        let original_manager = test_manager(workspace.path().to_path_buf());
        let mut original_agent = original_manager.create_agent();
        original_agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: lunar pine."),
            Message::assistant("Stored. I will remember lunar pine."),
        ]);
        persist_agent_history_to_store(
            &original_manager.history_store,
            chat_id,
            &original_agent,
            None,
        );

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let mut next_agent = restarted_manager.create_agent();
        next_agent.restore_conversation_messages(vec![
            Message::user("What exact phrase did I ask you to remember before the restart?"),
            Message::assistant("lunar pine"),
        ]);

        persist_agent_history_to_store(
            &restarted_manager.history_store,
            chat_id,
            &next_agent,
            None,
        );

        let persisted = restarted_manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 4);
        assert_eq!(
            persisted[0].as_text(),
            Some("Remember this exact phrase: lunar pine.")
        );
        assert_eq!(
            persisted[1].as_text(),
            Some("Stored. I will remember lunar pine.")
        );
        assert_eq!(
            persisted[2].as_text(),
            Some("What exact phrase did I ask you to remember before the restart?")
        );
        assert_eq!(persisted[3].as_text(), Some("lunar pine"));
    }

    #[test]
    fn test_reset_chat_clears_persisted_history_file() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 9001;
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember the answer is 17."),
            Message::assistant("Stored."),
        ]);
        manager.persist_agent_history(chat_id, &agent);
        let history_path = workspace
            .path()
            .join(".topagent")
            .join("telegram-history")
            .join("chat-9001.json");
        let memory_index_path = workspace.path().join(".topagent").join("MEMORY.md");
        assert!(history_path.is_file());
        assert!(memory_index_path.is_file());

        manager.sessions.insert(
            chat_id,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
            },
        );
        manager.reset_chat(chat_id);

        assert!(!history_path.exists());
        assert!(memory_index_path.exists());
        assert!(!manager.sessions.contains_key(&chat_id));
    }

    #[test]
    fn test_reset_chat_preserves_operator_and_workspace_memory_artifacts() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 9102;
        let mut manager = test_manager(workspace.path().to_path_buf());

        std::fs::create_dir_all(workspace.path().join(".topagent")).unwrap();
        std::fs::write(
            workspace.path().join(".topagent/USER.md"),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
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
        )
        .unwrap();
        manager.memory.consolidate_memory_if_needed().unwrap();

        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember the answer is 17."),
            Message::assistant("Stored."),
        ]);
        manager.persist_agent_history(chat_id, &agent);

        let transcript_path = workspace
            .path()
            .join(".topagent")
            .join("telegram-history")
            .join("chat-9102.json");
        let user_path = workspace.path().join(".topagent").join("USER.md");
        let memory_index_path = workspace.path().join(".topagent").join("MEMORY.md");
        let procedure_dir = workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR);
        assert!(transcript_path.exists());
        assert!(user_path.exists());
        assert!(memory_index_path.exists());
        assert!(procedure_dir.is_dir());

        manager.reset_chat(chat_id);

        assert!(!transcript_path.exists());
        assert!(user_path.exists());
        assert!(memory_index_path.exists());
        assert!(procedure_dir.is_dir());
    }

    #[test]
    fn test_create_agent_respects_global_tool_authoring_setting() {
        let workspace = TempDir::new().unwrap();
        let disabled_manager = test_manager(workspace.path().to_path_buf());
        let disabled_specs = disabled_manager.create_agent().tool_specs();
        assert!(!disabled_specs.iter().any(|spec| spec.name == "create_tool"));

        let enabled_manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default().with_generated_tool_authoring(true),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
        );
        let enabled_specs = enabled_manager.create_agent().tool_specs();
        assert!(enabled_specs.iter().any(|spec| spec.name == "create_tool"));
        assert!(enabled_specs.iter().any(|spec| spec.name == "repair_tool"));
    }

    #[test]
    fn test_reset_chat_does_not_create_tool_authoring_settings_state() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 3003;
        let mut manager = test_manager(workspace.path().to_path_buf());

        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember the answer is 17."),
            Message::assistant("Stored."),
        ]);
        manager.persist_agent_history(chat_id, &agent);

        let history_path = workspace
            .path()
            .join(".topagent")
            .join("telegram-history")
            .join("chat-3003.json");
        let settings_path = workspace
            .path()
            .join(".topagent")
            .join("telegram-settings")
            .join("chat-3003.json");
        assert!(history_path.is_file());
        assert!(!settings_path.exists());

        manager.reset_chat(chat_id);

        assert!(!history_path.exists());
        assert!(!settings_path.exists());
    }

    #[test]
    fn test_clear_workspace_telegram_history_removes_all_chat_transcripts() {
        let workspace = TempDir::new().unwrap();
        let history_store = ChatHistoryStore::new(workspace.path().to_path_buf());
        history_store
            .save(101, &[Message::user("hello"), Message::assistant("world")])
            .unwrap();
        history_store
            .save(202, &[Message::user("goodbye"), Message::assistant("moon")])
            .unwrap();

        let history_dir = workspace.path().join(".topagent").join("telegram-history");
        assert!(history_dir.is_dir());

        let cleared = clear_workspace_telegram_history(workspace.path()).unwrap();

        assert!(cleared);
        assert!(!history_dir.exists());
    }
}

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, create_provider, model::ModelRoute, tools::default_tools, Agent,
    ApprovalEntry, ApprovalMailbox, ApprovalMailboxMode, CancellationToken, Message,
    ProgressCallback, ProgressUpdate, Role, RuntimeOptions, TelegramAdapter, POLL_TIMEOUT_SECS,
};
use tracing::{error, info, warn};

use crate::config::*;
use crate::managed_files::write_managed_file;
use crate::memory::WorkspaceMemory;
use crate::progress::LiveProgress;

const TELEGRAM_HISTORY_VERSION: u32 = 1;
const TELEGRAM_CHAT_SETTINGS_VERSION: u32 = 1;
const MAX_PERSISTED_TRANSCRIPT_MESSAGES: usize = 100;

pub(crate) fn run_telegram(token: Option<String>, params: CliParams) -> Result<()> {
    let config =
        resolve_telegram_mode_config(token, params, TelegramModeDefaults::from_process_env())?;
    let token = config.token;
    let workspace = config.workspace;
    // Register known secrets for redaction in tool output and final replies.
    let mut secrets = topagent_core::SecretRegistry::new();
    secrets.register(&config.api_key);
    secrets.register(&token);
    let ctx = ExecutionContext::new(workspace).with_secrets(secrets.clone());
    let workspace_label = ctx.workspace_root.display().to_string();
    let options = config.options;
    let api_key = config.api_key;
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
        "starting Telegram mode | provider: {} | model: {} | workspace: {}",
        route.provider_id, route.model_id, workspace_label
    );
    info!(
        "bot: @{} (id: {}) | private text chats only | send /start in a private chat",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
    );

    let provider_label = route.provider_id.clone();
    let model_label = route.model_id.clone();
    let mut session_manager = ChatSessionManager::new(
        route,
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
        match adapter.get_updates(Some(offset), Some(POLL_TIMEOUT_SECS), Some(&["message"])) {
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
                    let Some(msg) = &update.message else { continue };
                    offset = update.update_id + 1;
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
                        let tool_authoring = if session_manager.chat_tool_authoring_enabled(chat_id)
                        {
                            "on"
                        } else {
                            "off"
                        };
                        let reply = format!(
                            "TopAgent\n\n\
                             Workspace: {}\n\
                             Provider: {} | Model: {}\n\
                             Tool authoring: {}\n\
                             Mode: private text chats only\n\n\
                             Commands:\n\
                             /help - show this message\n\
                             /stop - stop the current task\n\
                             /approvals - list pending approvals for this chat\n\
                             /approve <id> - approve a pending action\n\
                             /deny <id> - deny a pending action\n\
                             /reset - clear this chat's saved transcript\n\
                             /tool_authoring on|off - enable or disable generated-tool authoring for this chat\n\n\
                             Send a plain text message to start a task.",
                            workspace_label, provider_label, model_label, tool_authoring
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

                    if let Some(argument) = text.strip_prefix("/tool_authoring") {
                        let argument = argument.trim();
                        let reply = match argument {
                            "" => format!(
                                "Tool authoring is currently {} for this chat. Use /tool_authoring on or /tool_authoring off.",
                                if session_manager.chat_tool_authoring_enabled(chat_id) {
                                    "on"
                                } else {
                                    "off"
                                }
                            ),
                            "on" | "off" => {
                                let enabled = argument == "on";
                                match session_manager.set_chat_tool_authoring(chat_id, enabled) {
                                    Ok(()) => {
                                        if session_manager.is_task_running(chat_id) {
                                            format!(
                                                "Tool authoring is now {} for this chat. The current task is still running with its previous setting; the change will apply to the next task.",
                                                argument
                                            )
                                        } else {
                                            format!(
                                                "Tool authoring is now {} for this chat.",
                                                argument
                                            )
                                        }
                                    }
                                    Err(err) => format!(
                                        "Failed to update tool authoring for this chat: {}",
                                        err
                                    ),
                                }
                            }
                            _ => {
                                "Usage: /tool_authoring on or /tool_authoring off".to_string()
                            }
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
) {
    for chunk in chunks {
        // Last-mile secret redaction before the message reaches Telegram.
        let text = match secrets {
            Some(reg) => reg.redact(&chunk).into_owned(),
            None => chunk,
        };
        if let Err(e) = adapter.send_message_to_chat(chat_id, &text) {
            error!("failed to send message: {}", e);
        }
    }
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
}

#[derive(Debug, Clone)]
struct ChatSettingsStore {
    settings_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedChatSettings {
    version: u32,
    tool_authoring_enabled: bool,
}

impl ChatSettingsStore {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            settings_dir: workspace_root.join(".topagent").join("telegram-settings"),
        }
    }

    fn path_for_chat(&self, chat_id: i64) -> PathBuf {
        self.settings_dir.join(format!("chat-{chat_id}.json"))
    }

    fn load_tool_authoring(&self, chat_id: i64) -> Result<Option<bool>> {
        let path = self.path_for_chat(chat_id);
        if !path.exists() {
            return Ok(None);
        }

        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let settings: PersistedChatSettings = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if settings.version != TELEGRAM_CHAT_SETTINGS_VERSION {
            return Err(anyhow::anyhow!(
                "unsupported Telegram chat settings version {} in {}",
                settings.version,
                path.display()
            ));
        }

        Ok(Some(settings.tool_authoring_enabled))
    }

    fn save_tool_authoring(&self, chat_id: i64, enabled: bool) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.settings_dir)
            .with_context(|| format!("failed to create {}", self.settings_dir.display()))?;
        let path = self.path_for_chat(chat_id);
        let settings = PersistedChatSettings {
            version: TELEGRAM_CHAT_SETTINGS_VERSION,
            tool_authoring_enabled: enabled,
        };
        let contents = serde_json::to_string_pretty(&settings)
            .with_context(|| format!("failed to encode {}", path.display()))?;
        write_managed_file(&path, &contents, true)?;
        Ok(path)
    }
}

use anyhow::Context;

// ── Session manager ──

pub(crate) struct ChatSessionManager {
    route: ModelRoute,
    api_key: String,
    options: RuntimeOptions,
    history_store: ChatHistoryStore,
    settings_store: ChatSettingsStore,
    tool_authoring_cache: RefCell<HashMap<i64, bool>>,
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
        api_key: String,
        options: RuntimeOptions,
        workspace_root: PathBuf,
        secrets: topagent_core::SecretRegistry,
    ) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        let memory = WorkspaceMemory::new(workspace_root.clone());
        if let Err(err) = memory.ensure_layout() {
            warn!(
                "failed to initialize workspace memory layout in {}: {}",
                workspace_root.display(),
                err
            );
        }
        if let Err(err) = memory.consolidate_index_if_needed() {
            warn!(
                "failed to consolidate workspace memory index in {}: {}",
                workspace_root.display(),
                err
            );
        }

        Self {
            route,
            api_key,
            options,
            history_store: ChatHistoryStore::new(workspace_root.clone()),
            settings_store: ChatSettingsStore::new(workspace_root),
            tool_authoring_cache: RefCell::new(HashMap::new()),
            memory,
            secrets,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
        }
    }

    #[cfg(test)]
    pub fn create_agent(&self) -> Agent {
        self.create_agent_for_chat(0)
    }

    fn create_agent_for_chat(&self, chat_id: i64) -> Agent {
        let tools = default_tools();
        let options = self.options_for_chat(chat_id);
        let provider = create_provider(
            &self.route,
            &self.api_key,
            tools.specs(),
            options.provider_timeout_secs,
        )
        .expect("failed to create provider");
        Agent::with_route(provider, self.route.clone(), tools.into_inner(), options)
    }

    fn options_for_chat(&self, chat_id: i64) -> RuntimeOptions {
        self.options
            .clone()
            .with_generated_tool_authoring(self.chat_tool_authoring_enabled(chat_id))
    }

    fn chat_tool_authoring_enabled(&self, chat_id: i64) -> bool {
        if let Some(enabled) = self.tool_authoring_cache.borrow().get(&chat_id).copied() {
            return enabled;
        }

        let enabled = match self.settings_store.load_tool_authoring(chat_id) {
            Ok(Some(enabled)) => enabled,
            Ok(None) => self.options.enable_generated_tool_authoring,
            Err(err) => {
                warn!(
                    "failed to load Telegram chat settings for chat {} from {}: {}",
                    chat_id,
                    self.settings_store.path_for_chat(chat_id).display(),
                    err
                );
                self.options.enable_generated_tool_authoring
            }
        };
        self.tool_authoring_cache
            .borrow_mut()
            .insert(chat_id, enabled);
        enabled
    }

    fn set_chat_tool_authoring(&mut self, chat_id: i64, enabled: bool) -> Result<()> {
        self.settings_store.save_tool_authoring(chat_id, enabled)?;
        self.tool_authoring_cache
            .borrow_mut()
            .insert(chat_id, enabled);
        Ok(())
    }

    fn build_memory_context(&self, chat_id: i64, instruction: &str) -> Option<String> {
        if let Err(err) = self.memory.consolidate_index_if_needed() {
            warn!(
                "failed to consolidate workspace memory index in {}: {}",
                self.history_store.history_dir.display(),
                err
            );
        }

        let transcript = match self.history_store.load(chat_id) {
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
        };

        match self.memory.build_prompt(instruction, Some(&transcript)) {
            Ok(memory_prompt) => memory_prompt.prompt,
            Err(err) => {
                warn!(
                    "failed to build workspace memory context for chat {} in {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
                None
            }
        }
    }

    fn build_run_context(
        &self,
        ctx: &ExecutionContext,
        chat_id: i64,
        instruction: &str,
    ) -> ExecutionContext {
        let mut run_ctx = ctx.clone();
        if let Some(memory_context) = self.build_memory_context(chat_id, instruction) {
            run_ctx = run_ctx.with_memory_context(memory_context);
        }
        run_ctx
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
        reply.push_str("\nReply with /approve <id> or /deny <id>.");
        reply
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

        let Some(task) = self.sessions.get(&chat_id) else {
            return "No task is currently running in this chat.".to_string();
        };

        let result = if approve {
            task.approval_mailbox
                .approve(id, Some("approved from Telegram".to_string()))
        } else {
            task.approval_mailbox
                .deny(id, Some("denied from Telegram".to_string()))
        };

        match result {
            Ok(entry) => format!(
                "Approval {} {}.",
                entry.request.id,
                if approve { "approved" } else { "denied" }
            ),
            Err(err) => format!("Could not update approval {}: {}", id, err),
        }
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
        let mut agent = self.create_agent_for_chat(chat_id);

        let cancel_token = CancellationToken::new();
        let approval_mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let approval_adapter = adapter.clone();
        let approval_secrets = self.secrets.clone();
        approval_mailbox.set_notifier(Arc::new(move |request| {
            let mut message = request.render_details();
            message.push_str(&format!(
                "\n\nReply with /approve {} or /deny {}.",
                request.id, request.id
            ));
            let chunks = topagent_core::channel::telegram::chunk_text(&message, 4000);
            send_telegram(&approval_adapter, chat_id, chunks, Some(&approval_secrets));
        }));
        let run_ctx = self.build_run_context(
            &ctx.clone()
                .with_cancel_token(cancel_token.clone())
                .with_approval_mailbox(approval_mailbox.clone()),
            chat_id,
            text,
        );
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
        let worker_secrets = self.secrets.clone();
        let adapter = adapter.clone();
        let instruction = text.to_string();

        thread::spawn(move || {
            let has_progress = worker_progress_callback.is_some();
            if let Some(callback) = &worker_progress_callback {
                agent.set_progress_callback(Some(callback.clone()));
            }

            let result = agent.run(&run_ctx, &instruction);
            agent.set_progress_callback(None);
            match &result {
                Ok(response) => {
                    persist_agent_history_to_store(&history_store, chat_id, &agent, Some(response))
                }
                Err(_) => persist_agent_history_to_store(&history_store, chat_id, &agent, None),
            }

            if let Some(progress) = progress {
                progress.wait();
            }

            match result {
                Ok(response) => {
                    let max_len = 4000;
                    let chunks = if response.len() <= max_len {
                        vec![response]
                    } else {
                        topagent_core::channel::telegram::chunk_text(&response, max_len)
                    };
                    send_telegram(&adapter, chat_id, chunks, Some(&worker_secrets));
                }
                Err(topagent_core::Error::Stopped(_)) => {}
                Err(e) => {
                    // When progress is active, the status message already shows the
                    // failure via ProgressUpdate::failed. Don't send a duplicate error.
                    if !has_progress {
                        send_telegram(
                            &adapter,
                            chat_id,
                            vec![format!("Error: {}", e)],
                            Some(&worker_secrets),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::{
        ApprovalCheck, ApprovalRequestDraft, ApprovalTriggerKind, CancellationToken, Message,
        ModelRoute, ProgressKind, ProgressUpdate,
    };

    fn test_manager(workspace_root: PathBuf) -> ChatSessionManager {
        ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
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
        assert!(updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update == &ProgressUpdate::stopping()));
    }

    #[test]
    fn test_stop_chat_returns_false_when_idle() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        assert!(!manager.stop_chat(42));
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
    fn test_memory_context_retrieves_targeted_transcript_snippet_instead_of_restoring_whole_history(
    ) {
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
        let memory_context = restarted_manager
            .build_memory_context(chat_id, "What was the maple phrase I mentioned earlier?")
            .unwrap();

        assert!(memory_context.contains("maple comet"));
        assert!(!memory_context.contains("cedar echo"));
        assert!(workspace
            .path()
            .join(".topagent")
            .join("telegram-history")
            .join("chat-4242.json")
            .is_file());

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
    fn test_tool_authoring_setting_persists_per_chat() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4242;
        let mut manager = test_manager(workspace.path().to_path_buf());

        assert!(!manager.chat_tool_authoring_enabled(chat_id));
        manager.set_chat_tool_authoring(chat_id, true).unwrap();
        assert!(manager.chat_tool_authoring_enabled(chat_id));
        assert!(!manager.chat_tool_authoring_enabled(chat_id + 1));

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        assert!(restarted_manager.chat_tool_authoring_enabled(chat_id));
        assert!(!restarted_manager.chat_tool_authoring_enabled(chat_id + 1));

        let settings_path = workspace
            .path()
            .join(".topagent")
            .join("telegram-settings")
            .join("chat-4242.json");
        assert!(settings_path.is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&settings_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn test_create_agent_for_chat_respects_tool_authoring_setting() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 7;
        let mut manager = test_manager(workspace.path().to_path_buf());

        let disabled_specs = manager.create_agent_for_chat(chat_id).tool_specs();
        assert!(!disabled_specs.iter().any(|spec| spec.name == "create_tool"));

        manager.set_chat_tool_authoring(chat_id, true).unwrap();
        let enabled_specs = manager.create_agent_for_chat(chat_id).tool_specs();
        assert!(enabled_specs.iter().any(|spec| spec.name == "create_tool"));
        assert!(enabled_specs.iter().any(|spec| spec.name == "repair_tool"));
    }

    #[test]
    fn test_reset_chat_preserves_tool_authoring_setting() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 3003;
        let mut manager = test_manager(workspace.path().to_path_buf());
        manager.set_chat_tool_authoring(chat_id, true).unwrap();

        let mut agent = manager.create_agent_for_chat(chat_id);
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
        assert!(settings_path.is_file());

        manager.reset_chat(chat_id);

        assert!(!history_path.exists());
        assert!(settings_path.exists());
        assert!(manager.chat_tool_authoring_enabled(chat_id));
    }
}

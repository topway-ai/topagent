use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use topagent_core::{
    channel::{ChannelAdapter, OutgoingMessage},
    context::ExecutionContext,
    create_provider,
    model::ModelRoute,
    tools::default_tools,
    Agent, CancellationToken, ProgressCallback, ProgressUpdate, RuntimeOptions, TelegramAdapter,
    POLL_TIMEOUT_SECS,
};
use tracing::{error, info, warn};

use crate::config::*;
use crate::managed_files::write_managed_file;
use crate::progress::LiveProgress;

const TELEGRAM_HISTORY_VERSION: u32 = 1;

pub(crate) fn run_telegram(token: Option<String>, params: CliParams) -> Result<()> {
    let config = resolve_telegram_mode_config(token, params)?;
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
                        let reply = format!(
                            "TopAgent\n\n\
                             Workspace: {}\n\
                             Provider: {} | Model: {}\n\
                             Mode: private text chats only\n\n\
                             Commands:\n\
                             /help - show this message\n\
                             /stop - stop the current task\n\
                             /reset - clear conversation history\n\n\
                             Send a plain text message to start a task.",
                            workspace_label, provider_label, model_label
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

                    if text == "/reset" {
                        let reply = if session_manager.is_task_running(chat_id) {
                            "A task is still running. Send /stop and wait for it to finish before /reset."
                                .to_string()
                        } else {
                            session_manager.reset_chat(chat_id);
                            "Conversation history cleared.".to_string()
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
        let outgoing = OutgoingMessage { chat_id, text };
        if let Err(e) = adapter.send_message(outgoing) {
            error!("failed to send message: {}", e);
        }
    }
}

fn persist_agent_history_to_store(history_store: &ChatHistoryStore, chat_id: i64, agent: &Agent) {
    let messages = agent.conversation_messages();
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

    match history_store.save(chat_id, &messages) {
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

use anyhow::Context;

// ── Session manager ──

pub(crate) struct ChatSessionManager {
    route: ModelRoute,
    api_key: String,
    options: RuntimeOptions,
    history_store: ChatHistoryStore,
    secrets: topagent_core::SecretRegistry,
    pub sessions: HashMap<i64, SessionState>,
    completed_tx: mpsc::Sender<CompletedChatTask>,
    completed_rx: mpsc::Receiver<CompletedChatTask>,
}

pub(crate) enum SessionState {
    Idle(Box<Agent>),
    Running(RunningChatTask),
}

pub(crate) struct RunningChatTask {
    pub cancel_token: CancellationToken,
    pub progress_callback: Option<ProgressCallback>,
}

struct CompletedChatTask {
    chat_id: i64,
    agent: Agent,
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
        Self {
            route,
            api_key,
            options,
            history_store: ChatHistoryStore::new(workspace_root),
            secrets,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
        }
    }

    pub fn create_agent(&self) -> Agent {
        let provider = create_provider(
            &self.route,
            &self.api_key,
            default_tools().specs(),
            self.options.provider_timeout_secs,
        )
        .expect("failed to create provider");
        let tools = default_tools();
        Agent::with_options(provider, tools.into_inner(), self.options.clone())
    }

    fn create_restored_agent(&self, chat_id: i64) -> Agent {
        let mut agent = self.create_agent();
        match self.history_store.load(chat_id) {
            Ok(messages) if !messages.is_empty() => {
                let restored_count = messages.len();
                let messages: Vec<_> = messages
                    .into_iter()
                    .map(|m| m.redact_secrets(&self.secrets))
                    .collect();
                agent.restore_conversation_messages(messages);
                info!(
                    "restored {} Telegram history messages for chat {} from {}",
                    restored_count,
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display()
                );
            }
            Ok(_) => {}
            Err(err) => {
                warn!(
                    "failed to restore Telegram history for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
            }
        }
        agent
    }

    pub fn persist_agent_history(&self, chat_id: i64, agent: &Agent) {
        persist_agent_history_to_store(&self.history_store, chat_id, agent);
    }

    fn collect_finished_tasks(&mut self) {
        while let Ok(task) = self.completed_rx.try_recv() {
            self.persist_agent_history(task.chat_id, &task.agent);
            self.sessions
                .insert(task.chat_id, SessionState::Idle(Box::new(task.agent)));
        }
    }

    fn is_task_running(&self, chat_id: i64) -> bool {
        matches!(self.sessions.get(&chat_id), Some(SessionState::Running(_)))
    }

    fn stop_chat(&mut self, chat_id: i64) -> bool {
        let Some(SessionState::Running(task)) = self.sessions.get(&chat_id) else {
            return false;
        };

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
        for session in self.sessions.values() {
            let SessionState::Running(task) = session else {
                continue;
            };

            if let Some(callback) = &task.progress_callback {
                callback(update.clone());
            }
        }
    }

    pub fn reset_chat(&mut self, chat_id: i64) {
        self.sessions.remove(&chat_id);
        match self.history_store.clear(chat_id) {
            Ok(true) => {
                info!(
                    "cleared Telegram history for chat {} from {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display()
                );
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    "failed to clear Telegram history for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
            }
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
        let mut agent = match self.sessions.remove(&chat_id) {
            Some(SessionState::Idle(agent)) => *agent,
            Some(SessionState::Running(task)) => {
                self.sessions.insert(chat_id, SessionState::Running(task));
                return vec![
                    "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                        .to_string(),
                ];
            }
            None => self.create_restored_agent(chat_id),
        };

        let cancel_token = CancellationToken::new();
        let run_ctx = ctx.clone().with_cancel_token(cancel_token.clone());
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
            persist_agent_history_to_store(&history_store, chat_id, &agent);

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

            let _ = completed_tx.send(CompletedChatTask { chat_id, agent });
        });

        self.sessions.insert(
            chat_id,
            SessionState::Running(RunningChatTask {
                cancel_token,
                progress_callback,
            }),
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::{CancellationToken, Message, ModelRoute, ProgressKind, ProgressUpdate};

    fn test_manager(workspace_root: PathBuf) -> ChatSessionManager {
        ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace_root,
            topagent_core::SecretRegistry::new(),
        )
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
            SessionState::Running(RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: Some(progress_callback),
            }),
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
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
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
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
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
    fn test_restart_restores_persisted_chat_history_for_new_manager() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4242;
        let original_manager = test_manager(workspace.path().to_path_buf());
        let mut original_agent = original_manager.create_agent();
        original_agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: maple comet."),
            Message::assistant("Stored. I will remember maple comet."),
        ]);
        persist_agent_history_to_store(&original_manager.history_store, chat_id, &original_agent);

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let restored_agent = restarted_manager.create_restored_agent(chat_id);
        let restored_messages = restored_agent.conversation_messages();

        assert_eq!(restored_messages.len(), 2);
        assert_eq!(
            restored_messages[0].as_text(),
            Some("Remember this exact phrase: maple comet.")
        );
        assert_eq!(
            restored_messages[1].as_text(),
            Some("Stored. I will remember maple comet.")
        );
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
    fn test_history_is_saved_to_disk_before_collect_finished_tasks() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 777;
        let manager = test_manager(workspace.path().to_path_buf());
        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: cedar echo."),
            Message::assistant("Stored. I will remember cedar echo."),
        ]);

        persist_agent_history_to_store(&manager.history_store, chat_id, &agent);

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
        persist_agent_history_to_store(&original_manager.history_store, chat_id, &original_agent);

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let mut restored_agent = restarted_manager.create_restored_agent(chat_id);
        let mut restored_messages = restored_agent.conversation_messages();
        assert_eq!(restored_messages.len(), 2);
        restored_messages.push(Message::user(
            "What exact phrase did I ask you to remember before the restart?",
        ));
        restored_messages.push(Message::assistant("lunar pine"));
        restored_agent.restore_conversation_messages(restored_messages);

        persist_agent_history_to_store(&restarted_manager.history_store, chat_id, &restored_agent);

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
        assert!(history_path.is_file());

        manager.sessions.insert(chat_id, SessionState::Idle(Box::new(agent)));
        manager.reset_chat(chat_id);

        assert!(!history_path.exists());
        assert!(!manager.sessions.contains_key(&chat_id));
    }
}

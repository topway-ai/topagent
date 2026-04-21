use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use topagent_core::Message;
use tracing::{info, warn};

use crate::managed_files::write_managed_file;
use crate::memory::TELEGRAM_HISTORY_RELATIVE_DIR;

const TELEGRAM_HISTORY_VERSION: u32 = 1;
const MAX_PERSISTED_TRANSCRIPT_MESSAGES: usize = 100;

pub(crate) fn build_persisted_transcript(
    messages: &[Message],
    final_response: Option<&str>,
) -> Vec<Message> {
    use topagent_core::Role;

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

pub(crate) fn persist_messages_to_store(
    history_store: &ChatHistoryStore,
    chat_id: i64,
    messages: &[Message],
) {
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

pub(crate) fn persist_visible_exchange_to_store(
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
pub(crate) fn persist_agent_history_to_store(
    history_store: &ChatHistoryStore,
    chat_id: i64,
    agent: &topagent_core::Agent,
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

#[derive(Debug, Clone)]
pub(crate) struct ChatHistoryStore {
    history_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedChatHistory {
    version: u32,
    messages: Vec<topagent_core::Message>,
}

impl ChatHistoryStore {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self {
            history_dir: workspace_root.join(TELEGRAM_HISTORY_RELATIVE_DIR),
        }
    }

    pub(crate) fn path_for_chat(&self, chat_id: i64) -> PathBuf {
        self.history_dir.join(format!("chat-{chat_id}.json"))
    }

    pub(crate) fn load(&self, chat_id: i64) -> Result<Vec<topagent_core::Message>> {
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

    pub(crate) fn save(
        &self,
        chat_id: i64,
        messages: &[topagent_core::Message],
    ) -> Result<PathBuf> {
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

    pub(crate) fn clear(&self, chat_id: i64) -> Result<bool> {
        let path = self.path_for_chat(chat_id);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        Ok(true)
    }

    pub(crate) fn clear_all(&self) -> Result<bool> {
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

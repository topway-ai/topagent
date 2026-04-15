use serde::{Deserialize, Serialize};
use tracing::warn;

const TELEGRAM_API_URL: &str = "https://api.telegram.org";

/// Server-side long-poll hold time passed to Telegram's getUpdates.
pub const POLL_TIMEOUT_SECS: i64 = 30;

/// HTTP timeout for the long-poll client. Must exceed POLL_TIMEOUT_SECS so the
/// client never kills the connection before Telegram responds to an idle poll.
const POLL_CLIENT_TIMEOUT_SECS: u64 = POLL_TIMEOUT_SECS as u64 + 15;

/// HTTP timeout for short-lived API calls (sendMessage, editMessageText, etc.).
const SEND_CLIENT_TIMEOUT_SECS: u64 = 15;

/// Maximum retries for transient HTTP failures on send/edit calls.
const SEND_MAX_RETRIES: usize = 3;

#[derive(thiserror::Error, Debug)]
pub enum ChannelError {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("telegram error: {0}")]
    Telegram(String),

    #[error("channel error: {0}")]
    Other(String),
}

impl From<reqwest::Error> for ChannelError {
    fn from(e: reqwest::Error) -> Self {
        ChannelError::Http(e.to_string())
    }
}

impl From<serde_json::Error> for ChannelError {
    fn from(e: serde_json::Error) -> Self {
        ChannelError::Parse(e.to_string())
    }
}

#[derive(Clone)]
pub struct TelegramAdapter {
    token: String,
    /// Long-timeout client used exclusively for getUpdates long-polling.
    poll_client: reqwest::blocking::Client,
    /// Short-timeout client used for sendMessage, editMessageText, etc.
    send_client: reqwest::blocking::Client,
}

fn build_client(timeout_secs: u64) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("failed to create HTTP client")
}

impl TelegramAdapter {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            poll_client: build_client(POLL_CLIENT_TIMEOUT_SECS),
            send_client: build_client(SEND_CLIENT_TIMEOUT_SECS),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", TELEGRAM_API_URL, self.token, method)
    }

    pub fn get_me(&self) -> Result<TelegramUser, ChannelError> {
        self.send_client
            .get(self.api_url("getMe"))
            .send()?
            .json::<TelegramResponse<TelegramUser>>()?
            .into_result()
    }

    pub fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: Option<i64>,
        allowed_updates: Option<&[&str]>,
    ) -> Result<Vec<TelegramUpdate>, ChannelError> {
        #[derive(Serialize)]
        struct GetUpdatesParams {
            offset: Option<i64>,
            timeout: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            allowed_updates: Option<Vec<String>>,
        }

        let params = GetUpdatesParams {
            offset,
            timeout: timeout_secs.or(Some(POLL_TIMEOUT_SECS)),
            allowed_updates: allowed_updates.map(|u| u.iter().map(|s| s.to_string()).collect()),
        };

        self.poll_client
            .get(self.api_url("getUpdates"))
            .json(&params)
            .send()?
            .json::<TelegramResponse<Vec<TelegramUpdate>>>()?
            .into_result()
    }

    pub fn send_message_to_chat(
        &self,
        chat_id: i64,
        text: &str,
    ) -> Result<TelegramMessage, ChannelError> {
        self.send_message_to_chat_with_markup(chat_id, text, None)
    }

    pub fn send_message_to_chat_with_markup(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramMessage, ChannelError> {
        #[derive(Serialize)]
        struct SendMessageParams {
            chat_id: i64,
            text: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_markup: Option<TelegramInlineKeyboardMarkup>,
        }

        let params = SendMessageParams {
            chat_id,
            text: text.to_string(),
            reply_markup: reply_markup.cloned(),
        };

        self.send_with_retry(|| {
            self.send_client
                .post(self.api_url("sendMessage"))
                .json(&params)
                .send()?
                .json::<TelegramResponse<TelegramMessage>>()?
                .into_result()
        })
    }

    pub fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<bool, ChannelError> {
        #[derive(Serialize)]
        struct AnswerCallbackQueryParams {
            callback_query_id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            text: Option<String>,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            show_alert: bool,
        }

        let params = AnswerCallbackQueryParams {
            callback_query_id: callback_query_id.to_string(),
            text: text.map(ToString::to_string),
            show_alert,
        };

        self.send_with_retry(|| {
            self.send_client
                .post(self.api_url("answerCallbackQuery"))
                .json(&params)
                .send()?
                .json::<TelegramResponse<bool>>()?
                .into_result()
        })
    }

    pub fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramMessage, ChannelError> {
        #[derive(Serialize)]
        struct EditMessageParams {
            chat_id: i64,
            message_id: i64,
            text: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            reply_markup: Option<TelegramInlineKeyboardMarkup>,
        }

        let params = EditMessageParams {
            chat_id,
            message_id,
            text: text.to_string(),
            reply_markup: reply_markup.cloned(),
        };

        self.send_with_retry(|| {
            self.send_client
                .post(self.api_url("editMessageText"))
                .json(&params)
                .send()?
                .json::<TelegramResponse<TelegramMessage>>()?
                .into_result()
        })
    }

    pub fn check_webhook(&self) -> Result<bool, ChannelError> {
        #[derive(Deserialize)]
        struct WebhookInfo {
            url: Option<String>,
        }

        let info: WebhookInfo = self
            .send_client
            .get(self.api_url("getWebhookInfo"))
            .send()?
            .json::<TelegramResponse<WebhookInfo>>()?
            .into_result()?;

        Ok(info.url.is_some_and(|url| !url.is_empty()))
    }

    /// Retries a send/edit closure on transient HTTP errors with exponential backoff.
    /// Telegram API errors (malformed request, etc.) are not retried.
    fn send_with_retry<T, F>(&self, f: F) -> Result<T, ChannelError>
    where
        F: Fn() -> Result<T, ChannelError>,
    {
        for attempt in 0..=SEND_MAX_RETRIES {
            match f() {
                Ok(msg) => return Ok(msg),
                Err(ChannelError::Http(msg)) if attempt < SEND_MAX_RETRIES => {
                    let backoff_ms = 500 * (1 << attempt); // 500ms, 1s, 2s
                    warn!(
                        "send failed (attempt {}): {}. Retrying in {}ms",
                        attempt + 1,
                        msg,
                        backoff_ms
                    );
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop always returns")
    }

    pub fn acknowledge(&self, _chat_id: i64, _message_id: i64) -> Result<(), ChannelError> {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    ok: bool,
    result: T,
    #[serde(rename = "description")]
    description: Option<String>,
}

impl<T> TelegramResponse<T> {
    fn into_result(self) -> Result<T, ChannelError> {
        if self.ok {
            Ok(self.result)
        } else {
            Err(ChannelError::Telegram(self.description.unwrap_or_default()))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramInlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<TelegramInlineKeyboardButton>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramInlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    #[serde(rename = "message")]
    pub message: Option<TelegramMessage>,
    #[serde(rename = "callback_query")]
    pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    pub text: Option<String>,
    #[serde(default)]
    pub from: Option<TelegramUser>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramCallbackQuery {
    pub id: String,
    pub message: Option<TelegramMessage>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

pub fn chunk_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current_chunk = String::new();

    for line in text.lines() {
        let line_len = line.len();

        if current_chunk.len() + line_len + 1 > max_len {
            if !current_chunk.is_empty() {
                chunks.push(current_chunk.clone());
                current_chunk.clear();
            }

            if line_len > max_len {
                let mut remaining = line;
                while !remaining.is_empty() {
                    let split_at = std::cmp::min(max_len, remaining.len());
                    let (chunk, rest) = remaining.split_at(split_at);
                    chunks.push(chunk.to_string());
                    remaining = rest;
                }
            } else {
                current_chunk.push_str(line);
            }
        } else {
            if !current_chunk.is_empty() && !line.is_empty() {
                current_chunk.push('\n');
            }
            current_chunk.push_str(line);
        }
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poll_client_timeout_exceeds_poll_hold_time() {
        assert!(
            POLL_CLIENT_TIMEOUT_SECS > POLL_TIMEOUT_SECS as u64,
            "POLL_CLIENT_TIMEOUT_SECS ({}) must exceed POLL_TIMEOUT_SECS ({})",
            POLL_CLIENT_TIMEOUT_SECS,
            POLL_TIMEOUT_SECS,
        );
    }

    #[test]
    fn test_send_client_timeout_is_short() {
        const { assert!(SEND_CLIENT_TIMEOUT_SECS <= 30) };
    }

    #[test]
    fn test_inline_keyboard_markup_serializes_callback_buttons() {
        let markup = TelegramInlineKeyboardMarkup {
            inline_keyboard: vec![vec![
                TelegramInlineKeyboardButton {
                    text: "Approve".to_string(),
                    callback_data: "approval:approve:apr-1".to_string(),
                },
                TelegramInlineKeyboardButton {
                    text: "Deny".to_string(),
                    callback_data: "approval:deny:apr-1".to_string(),
                },
            ]],
        };

        let value = serde_json::to_value(&markup).unwrap();
        assert_eq!(
            value["inline_keyboard"][0][0]["callback_data"],
            "approval:approve:apr-1"
        );
        assert_eq!(value["inline_keyboard"][0][1]["text"], "Deny");
    }

    #[test]
    fn test_chunk_text_short() {
        let text = "short text";
        let chunks = chunk_text(text, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short text");
    }

    #[test]
    fn test_chunk_text_exactly_limit() {
        let text = "exactly100 chars here!___________________________________________________";
        let chunks = chunk_text(text, 50);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn test_chunk_text_multiline() {
        let text = "line1\nline2\nline3";
        let chunks = chunk_text(text, 100);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_chunk_text_very_long_line() {
        let text = "a".repeat(200);
        let chunks = chunk_text(&text, 50);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 50);
        }
    }

    #[test]
    fn test_chunk_text_exact_boundary() {
        let text = "123456789012345"; // 15 chars
        let chunks = chunk_text(text, 15);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "123456789012345");
    }

    #[test]
    fn test_chunk_text_one_over_boundary() {
        let text = "1234567890123456"; // 16 chars
        let chunks = chunk_text(text, 15);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "123456789012345");
        assert_eq!(chunks[1], "6");
    }

    #[test]
    fn test_chunk_text_empty() {
        let chunks = chunk_text("", 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn test_chunk_text_two_lines_exact() {
        let text = "line1\nline2";
        let chunks = chunk_text(text, 50);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "line1\nline2");
    }

    #[test]
    fn test_chunk_text_nearly_empty_lines() {
        let text = "\n\n";
        let chunks = chunk_text(text, 10);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_chunk_text_all_whitespace() {
        let text = "     ";
        let chunks = chunk_text(text, 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "     ");
    }
}

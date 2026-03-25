use super::adapter::{ChannelAdapter, ChannelError, IncomingMessage, OutgoingMessage};
use serde::{Deserialize, Serialize};

const TELEGRAM_API_URL: &str = "https://api.telegram.org";

#[derive(Clone)]
pub struct TelegramAdapter {
    token: String,
    client: reqwest::blocking::Client,
}

impl TelegramAdapter {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to create HTTP client"),
        }
    }

    pub fn with_timeout(token: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            token: token.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .expect("failed to create HTTP client"),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", TELEGRAM_API_URL, self.token, method)
    }

    pub fn get_me(&self) -> Result<TelegramUser, ChannelError> {
        let response = self
            .client
            .get(self.api_url("getMe"))
            .send()?
            .json::<TelegramResponse<TelegramUser>>()?;

        if !response.ok {
            return Err(ChannelError::Telegram(
                response.description.unwrap_or_default(),
            ));
        }
        Ok(response.result)
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
            timeout: timeout_secs.or(Some(30)),
            allowed_updates: allowed_updates.map(|u| u.iter().map(|s| s.to_string()).collect()),
        };

        let response = self
            .client
            .get(self.api_url("getUpdates"))
            .json(&params)
            .send()?
            .json::<TelegramResponse<Vec<TelegramUpdate>>>()?;

        if !response.ok {
            return Err(ChannelError::Telegram(
                response.description.unwrap_or_default(),
            ));
        }

        Ok(response.result)
    }

    pub fn send_message_to_chat(
        &self,
        chat_id: i64,
        text: &str,
    ) -> Result<TelegramMessage, ChannelError> {
        #[derive(Serialize)]
        struct SendMessageParams {
            chat_id: i64,
            text: String,
        }

        let params = SendMessageParams {
            chat_id,
            text: text.to_string(),
        };

        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&params)
            .send()?
            .json::<TelegramResponse<TelegramMessage>>()?;

        if !response.ok {
            return Err(ChannelError::Telegram(
                response.description.unwrap_or_default(),
            ));
        }

        Ok(response.result)
    }

    pub fn check_webhook(&self) -> Result<bool, ChannelError> {
        #[derive(Deserialize)]
        struct WebhookInfo {
            url: Option<String>,
        }

        let response = self
            .client
            .get(self.api_url("getWebhookInfo"))
            .send()?
            .json::<TelegramResponse<WebhookInfo>>()?;

        if !response.ok {
            return Err(ChannelError::Telegram(
                response.description.unwrap_or_default(),
            ));
        }

        Ok(response.result.url.is_some_and(|url| !url.is_empty()))
    }
}

impl ChannelAdapter for TelegramAdapter {
    fn fetch_messages(&self) -> Result<Vec<IncomingMessage>, ChannelError> {
        let updates = self.get_updates(None, Some(30), None)?;
        let messages: Vec<IncomingMessage> = updates
            .into_iter()
            .filter_map(|update| {
                let msg = update.message?;
                if msg.chat.chat_type != "private" {
                    return None;
                }
                let text = msg.text?;
                Some(IncomingMessage {
                    chat_id: msg.chat.id,
                    text: Some(text),
                    message_id: msg.message_id,
                    is_command: false,
                })
            })
            .collect();
        Ok(messages)
    }

    fn send_message(&self, msg: OutgoingMessage) -> Result<(), ChannelError> {
        self.send_message_to_chat(msg.chat_id, &msg.text)?;
        Ok(())
    }

    fn acknowledge(&self, _chat_id: i64, _message_id: i64) -> Result<(), ChannelError> {
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
}

#[derive(Debug, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    pub text: Option<String>,
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

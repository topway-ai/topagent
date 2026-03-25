#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub chat_id: i64,
    pub text: String,
    pub message_id: i64,
}

#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    pub chat_id: i64,
    pub text: String,
}

pub trait ChannelAdapter: Send + Sync {
    fn fetch_messages(&self) -> Result<Vec<IncomingMessage>, ChannelError>;
    fn send_message(&self, msg: OutgoingMessage) -> Result<(), ChannelError>;
    fn acknowledge(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError>;
}

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

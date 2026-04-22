use topagent_core::channel::telegram::{ChannelError, TelegramInlineKeyboardMarkup};
use topagent_core::TelegramAdapter;
use tracing::error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeliveryReport {
    pub attempted_chunks: usize,
    pub delivered_chunks: usize,
    pub first_error: Option<String>,
}

impl DeliveryReport {
    pub(crate) fn fully_delivered(&self) -> bool {
        self.attempted_chunks > 0 && self.delivered_chunks == self.attempted_chunks
    }
}

pub(crate) trait TelegramOutbound {
    fn send_message_to_chat(&self, chat_id: i64, text: &str) -> Result<(), ChannelError>;
    fn send_message_to_chat_with_markup(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<(), ChannelError>;
    fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), ChannelError>;
    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<(), ChannelError>;
    fn acknowledge(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError>;
}

impl TelegramOutbound for TelegramAdapter {
    fn send_message_to_chat(&self, chat_id: i64, text: &str) -> Result<(), ChannelError> {
        TelegramAdapter::send_message_to_chat(self, chat_id, text).map(|_| ())
    }

    fn send_message_to_chat_with_markup(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<(), ChannelError> {
        TelegramAdapter::send_message_to_chat_with_markup(self, chat_id, text, reply_markup)
            .map(|_| ())
    }

    fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), ChannelError> {
        TelegramAdapter::answer_callback_query(self, callback_query_id, text, show_alert)
            .map(|_| ())
    }

    fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<(), ChannelError> {
        TelegramAdapter::edit_message_text(self, chat_id, message_id, text, reply_markup)
            .map(|_| ())
    }

    fn acknowledge(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError> {
        TelegramAdapter::acknowledge(self, chat_id, message_id)
    }
}

pub(crate) fn send_telegram<T: TelegramOutbound + ?Sized>(
    adapter: &T,
    chat_id: i64,
    chunks: Vec<String>,
    secrets: Option<&topagent_core::SecretRegistry>,
) -> DeliveryReport {
    send_telegram_with_markup(adapter, chat_id, chunks, None, secrets)
}

pub(crate) fn send_telegram_with_markup<T: TelegramOutbound + ?Sized>(
    adapter: &T,
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

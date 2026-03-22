use crate::{Error, Message, Result};
use std::sync::{Arc, RwLock};

pub trait Provider: Send + Sync {
    fn complete(&self, messages: &[Message]) -> Result<ProviderResponse>;
}

#[derive(Debug, Clone)]
pub enum ProviderResponse {
    Message(Message),
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    RequiresInput,
}

pub struct ScriptedProvider {
    responses: Vec<ProviderResponse>,
    index: Arc<RwLock<usize>>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<ProviderResponse>) -> Self {
        Self {
            responses,
            index: Arc::new(RwLock::new(0)),
        }
    }
}

impl Provider for ScriptedProvider {
    fn complete(&self, _messages: &[Message]) -> Result<ProviderResponse> {
        let mut idx = self.index.write().unwrap();
        if let Some(r) = self.responses.get(*idx).cloned() {
            *idx += 1;
            Ok(r)
        } else {
            Err(Error::Provider("scripted provider exhausted".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, Role};

    #[test]
    fn test_scripted_provider_returns_responses_in_order() {
        let responses = vec![
            ProviderResponse::Message(Message {
                role: Role::Assistant,
                content: Content::Text {
                    text: "first".into(),
                },
            }),
            ProviderResponse::Message(Message {
                role: Role::Assistant,
                content: Content::Text {
                    text: "second".into(),
                },
            }),
        ];
        let provider = ScriptedProvider::new(responses);

        let result1 = provider.complete(&[]).unwrap();
        let result2 = provider.complete(&[]).unwrap();

        assert!(matches!(result1, ProviderResponse::Message(_)));
        assert!(matches!(result2, ProviderResponse::Message(_)));
    }

    #[test]
    fn test_scripted_provider_exhausted_error() {
        let responses = vec![ProviderResponse::Message(Message {
            role: Role::Assistant,
            content: Content::Text {
                text: "only one".into(),
            },
        })];
        let provider = ScriptedProvider::new(responses);

        provider.complete(&[]).unwrap();
        let result = provider.complete(&[]);
        assert!(result.is_err());
    }
}

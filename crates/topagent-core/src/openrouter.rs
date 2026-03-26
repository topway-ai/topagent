use crate::tool_spec::ToolSpec;
use crate::{Content, Error, Message, Provider, ProviderResponse, Result, Role, ToolCallEntry};
use serde::{Deserialize, Serialize};

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    api_key: String,
    client: reqwest::blocking::Client,
    tools: Vec<ToolSpec>,
}

impl OpenRouterProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let _ = model; // model is determined by route at call time
        Self::with_timeout(api_key, 120)
    }

    pub fn with_timeout(api_key: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            api_key: api_key.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .expect("failed to create HTTP client"),
            tools: crate::tools::default_tools().specs(),
        }
    }

    pub fn with_tools(
        api_key: impl Into<String>,
        model: impl Into<String>,
        tools: Vec<ToolSpec>,
    ) -> Self {
        let _ = model; // model is determined by route at call time
        Self::with_tools_and_timeout(api_key, tools, 120)
    }

    pub fn with_tools_and_timeout(
        api_key: impl Into<String>,
        tools: Vec<ToolSpec>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .expect("failed to create HTTP client"),
            tools,
        }
    }
}

impl Provider for OpenRouterProvider {
    fn complete(
        &self,
        messages: &[Message],
        route: &crate::ModelRoute,
    ) -> Result<ProviderResponse> {
        let request = self.build_request(messages, &route.model_id);
        let response = self
            .client
            .post(format!("{}/chat/completions", OPENROUTER_BASE_URL))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .map_err(|e| Error::ProviderRequestFailed(format!("request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(Error::ProviderRequestFailed(format!(
                "API error {}: {}",
                status, body
            )));
        }

        let completion: OpenAIResponse = response
            .json()
            .map_err(|e| Error::ProviderParseFailed(format!("failed to parse response: {}", e)))?;

        self.parse_response(completion)
    }
}

impl OpenRouterProvider {
    pub(crate) fn build_request(&self, messages: &[Message], model_id: &str) -> ChatRequest {
        let wire_messages: Vec<WireMessage> = messages.iter().map(message_to_wire).collect();

        let tools: Vec<ToolDefinition> = self
            .tools
            .iter()
            .map(|spec| ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: spec.name.to_string(),
                    description: spec.description.to_string(),
                    parameters: spec.input_schema.clone(),
                },
            })
            .collect();

        ChatRequest {
            model: model_id.to_string(),
            messages: wire_messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: Some(serde_json::json!({"type": "auto"})),
        }
    }

    pub(crate) fn parse_response(&self, response: OpenAIResponse) -> Result<ProviderResponse> {
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::ProviderParseFailed("no choices in response".into()))?;

        let message = choice.message;

        if let Some(tool_calls) = message.tool_calls {
            let count = tool_calls.len();
            if count == 1 {
                let tool_call = tool_calls.into_iter().next().unwrap();
                let id = tool_call.id;
                let function = tool_call.function;
                let name = function.name;
                let args: serde_json::Value =
                    serde_json::from_str(&function.arguments).map_err(|e| {
                        Error::ProviderParseFailed(format!("failed to parse tool args: {}", e))
                    })?;
                return Ok(ProviderResponse::ToolCall { id, name, args });
            }
            if count > 1 {
                let mut entries = Vec::with_capacity(count);
                for tool_call in tool_calls {
                    let id = tool_call.id;
                    let function = tool_call.function;
                    let name = function.name;
                    let args: serde_json::Value = serde_json::from_str(&function.arguments)
                        .map_err(|e| {
                            Error::ProviderParseFailed(format!("failed to parse tool args: {}", e))
                        })?;
                    entries.push(ToolCallEntry { id, name, args });
                }
                return Ok(ProviderResponse::ToolCalls(entries));
            }
        }

        let content = message.content.unwrap_or_default();
        Ok(ProviderResponse::Message(Message {
            role: Role::Assistant,
            content: Content::Text { text: content },
        }))
    }
}

fn message_to_wire(msg: &Message) -> WireMessage {
    match (&msg.role, &msg.content) {
        (Role::User, Content::Text { text }) => WireMessage {
            role: "user".to_string(),
            content: Some(WireContent::Text(text.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        (Role::System, Content::Text { text }) => WireMessage {
            role: "system".to_string(),
            content: Some(WireContent::Text(text.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        (Role::Assistant, Content::Text { text }) => WireMessage {
            role: "assistant".to_string(),
            content: Some(WireContent::Text(text.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        (Role::Assistant, Content::ToolRequest { id, name, args }) => WireMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![WireToolCall {
                id: id.clone(),
                function: WireFunctionCall {
                    name: name.clone(),
                    arguments: args.to_string(),
                },
            }]),
            tool_call_id: None,
        },
        (Role::Tool, Content::ToolResult { id, result }) => WireMessage {
            role: "tool".to_string(),
            content: Some(WireContent::Text(result.clone())),
            tool_calls: None,
            tool_call_id: Some(id.clone()),
        },
        _ => WireMessage {
            role: "user".to_string(),
            content: Some(WireContent::Text(String::new())),
            tool_calls: None,
            tool_call_id: None,
        },
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatRequest {
    model: String,
    messages: Vec<WireMessage>,
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<WireContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
}

#[derive(Debug, Serialize)]
struct WireToolCall {
    id: String,
    function: WireFunctionCall,
}

#[derive(Debug, Serialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: FunctionDefinition,
}

#[derive(Debug, Serialize)]
struct FunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAIResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "function")]
    function: FunctionCall,
}

#[derive(Debug, Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::default_tools;

    #[test]
    fn test_build_request_uses_default_tools() {
        let provider = OpenRouterProvider::new("test-key", "test-model");
        let messages = vec![Message::user("test")];
        let request = provider.build_request(&messages, "test-model");

        let tools = request.tools.unwrap();
        assert_eq!(tools.len(), 9);
        let names: Vec<_> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "read",
                "write",
                "edit",
                "bash",
                "git_status",
                "git_diff",
                "git_branch",
                "git_add",
                "git_commit"
            ]
        );
    }

    #[test]
    fn test_parse_text_response() {
        let provider = OpenRouterProvider::new("key", "model");
        let response = OpenAIResponse {
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello, world!".to_string()),
                    tool_calls: None,
                },
            }],
        };
        let result = provider.parse_response(response).unwrap();
        match result {
            ProviderResponse::Message(msg) => {
                assert_eq!(msg.as_text().unwrap(), "Hello, world!");
            }
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn test_parse_tool_call_response() {
        let provider = OpenRouterProvider::new("key", "model");
        let response = OpenAIResponse {
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".to_string(),
                        function: FunctionCall {
                            name: "read".to_string(),
                            arguments: r#"{"path": "test.txt"}"#.to_string(),
                        },
                    }]),
                },
            }],
        };
        let result = provider.parse_response(response).unwrap();
        match result {
            ProviderResponse::ToolCall { id, name, args } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "read");
                assert_eq!(args["path"], "test.txt");
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn test_parse_malformed_tool_args_fails() {
        let provider = OpenRouterProvider::new("key", "model");
        let response = OpenAIResponse {
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".to_string(),
                        function: FunctionCall {
                            name: "read".to_string(),
                            arguments: "not json".to_string(),
                        },
                    }]),
                },
            }],
        };
        let result = provider.parse_response(response);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_multiple_tool_calls_returns_success() {
        let provider = OpenRouterProvider::new("key", "model");
        let response = OpenAIResponse {
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![
                        ToolCall {
                            id: "call_1".to_string(),
                            function: FunctionCall {
                                name: "read".to_string(),
                                arguments: r#"{"path": "a.txt"}"#.to_string(),
                            },
                        },
                        ToolCall {
                            id: "call_2".to_string(),
                            function: FunctionCall {
                                name: "read".to_string(),
                                arguments: r#"{"path": "b.txt"}"#.to_string(),
                            },
                        },
                    ]),
                },
            }],
        };
        let result = provider.parse_response(response);
        assert!(result.is_ok());
        match result.unwrap() {
            ProviderResponse::ToolCalls(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "read");
                assert_eq!(calls[0].args["path"], "a.txt");
                assert_eq!(calls[1].id, "call_2");
                assert_eq!(calls[1].name, "read");
                assert_eq!(calls[1].args["path"], "b.txt");
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[test]
    fn test_tool_order_deterministic() {
        let specs = default_tools().specs();
        let provider = OpenRouterProvider::with_tools("key", "model", specs);
        let messages = vec![Message::user("test")];
        let request1 = provider.build_request(&messages, "model");
        let request2 = provider.build_request(&messages, "model");

        let tools1 = request1.tools.unwrap();
        let tools2 = request2.tools.unwrap();
        assert_eq!(tools1.len(), tools2.len());
        for (t1, t2) in tools1.iter().zip(tools2.iter()) {
            assert_eq!(t1.function.name, t2.function.name);
        }
    }

    #[test]
    fn test_wire_message_user_text() {
        let msg = Message::user("hello");
        let wire = message_to_wire(&msg);
        assert_eq!(wire.role, "user");
        assert!(wire.content.is_some());
        assert!(wire.tool_calls.is_none());
    }

    #[test]
    fn test_wire_message_assistant_tool_request() {
        let msg = Message::tool_request("call_1", "read", serde_json::json!({"path": "foo.txt"}));
        let wire = message_to_wire(&msg);
        assert_eq!(wire.role, "assistant");
        assert!(wire.content.is_none());
        assert!(wire.tool_calls.is_some());
        let tc = &wire.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.function.name, "read");
    }

    #[test]
    fn test_wire_message_tool_result() {
        let msg = Message::tool_result("call_1", "file contents here");
        let wire = message_to_wire(&msg);
        assert_eq!(wire.role, "tool");
        assert!(wire.tool_call_id.as_ref().unwrap() == "call_1");
        assert!(wire.content.is_some());
    }
}

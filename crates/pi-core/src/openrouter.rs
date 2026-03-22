use crate::tool_spec::ToolSpec;
use crate::{tools::all_specs, Content, Error, Message, Provider, ProviderResponse, Result, Role};
use serde::{Deserialize, Serialize};

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
    tools: Vec<ToolSpec>,
}

impl OpenRouterProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("failed to create HTTP client"),
            tools: all_specs(),
        }
    }

    pub fn with_tools(
        api_key: impl Into<String>,
        model: impl Into<String>,
        tools: Vec<ToolSpec>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("failed to create HTTP client"),
            tools,
        }
    }
}

impl Provider for OpenRouterProvider {
    fn complete(&self, messages: &[Message]) -> Result<ProviderResponse> {
        let request = self.build_request(messages);
        let response = self
            .client
            .post(format!("{}/chat/completions", OPENROUTER_BASE_URL))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .map_err(|e| Error::Provider(format!("request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(Error::Provider(format!("API error {}: {}", status, body)));
        }

        let completion: OpenAIResponse = response
            .json()
            .map_err(|e| Error::Provider(format!("failed to parse response: {}", e)))?;

        self.parse_response(completion)
    }
}

impl OpenRouterProvider {
    fn build_request(&self, messages: &[Message]) -> ChatRequest {
        let messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| ChatMessage {
                role: match m.role {
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                    Role::System => "system".to_string(),
                    Role::Tool => "tool".to_string(),
                },
                content: match &m.content {
                    Content::Text { text } => text.clone(),
                    Content::ToolRequest { name, args, .. } => {
                        serde_json::json!({"type": "tool_call", "name": name, "args": args})
                            .to_string()
                    }
                    Content::ToolResult { result, .. } => result.clone(),
                },
            })
            .collect();

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
            model: self.model.clone(),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: Some(serde_json::json!({"type": "auto"})),
        }
    }

    fn parse_response(&self, response: OpenAIResponse) -> Result<ProviderResponse> {
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Provider("no choices in response".into()))?;

        let message = choice.message;

        if let Some(tool_calls) = message.tool_calls {
            if let Some(tool_call) = tool_calls.into_iter().next() {
                let id = tool_call.id;
                let function = tool_call.function;
                let name = function.name;
                let args: serde_json::Value = serde_json::from_str(&function.arguments)
                    .map_err(|e| Error::Provider(format!("failed to parse tool args: {}", e)))?;
                return Ok(ProviderResponse::ToolCall { id, name, args });
            }
        }

        let content = message.content.unwrap_or_default();
        Ok(ProviderResponse::Message(Message {
            role: Role::Assistant,
            content: Content::Text { text: content },
        }))
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
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
struct OpenAIResponse {
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

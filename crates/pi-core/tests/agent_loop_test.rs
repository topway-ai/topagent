use pi_core::{
    context::ExecutionContext,
    tools::{BashTool, EditTool, ReadTool, Tool, WriteTool},
    Agent, Content, Error, Message, Provider, ProviderResponse, Role, RuntimeOptions,
};
use std::sync::{Arc, RwLock};
use tempfile::TempDir;

fn make_test_context() -> (ExecutionContext, TempDir) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    (ExecutionContext::new(root), temp)
}

fn make_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadTool::new()) as Box<dyn Tool>,
        Box::new(WriteTool::new()) as Box<dyn Tool>,
        Box::new(EditTool::new()) as Box<dyn Tool>,
        Box::new(BashTool::new()) as Box<dyn Tool>,
    ]
}

struct TransientFailProvider {
    fail_count: Arc<RwLock<usize>>,
    responses: Vec<ProviderResponse>,
    response_idx: Arc<RwLock<usize>>,
}

impl TransientFailProvider {
    fn new(fail_count: usize, responses: Vec<ProviderResponse>) -> Self {
        Self {
            fail_count: Arc::new(RwLock::new(fail_count)),
            responses,
            response_idx: Arc::new(RwLock::new(0)),
        }
    }
}

impl Provider for TransientFailProvider {
    fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
        let mut fail_count = self.fail_count.write().unwrap();
        if *fail_count > 0 {
            *fail_count -= 1;
            return Err(Error::Provider("transient failure".into()));
        }
        drop(fail_count);

        let mut idx = self.response_idx.write().unwrap();
        if let Some(r) = self.responses.get(*idx).cloned() {
            *idx += 1;
            Ok(r)
        } else {
            Err(Error::Provider("provider exhausted".into()))
        }
    }
}

struct EmptyResponseProvider {
    remaining: Arc<RwLock<usize>>,
    fallback: ProviderResponse,
}

impl EmptyResponseProvider {
    fn new(empty_count: usize, fallback: ProviderResponse) -> Self {
        Self {
            remaining: Arc::new(RwLock::new(empty_count)),
            fallback,
        }
    }
}

impl Provider for EmptyResponseProvider {
    fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
        let mut remaining = self.remaining.write().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            Ok(ProviderResponse::Message(Message {
                role: Role::Assistant,
                content: Content::Text {
                    text: String::new(),
                },
            }))
        } else {
            Ok(self.fallback.clone())
        }
    }
}

struct RunawayProvider;

impl Provider for RunawayProvider {
    fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
        Ok(ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "echo loop"}),
        })
    }
}

struct MalformedArgsProvider {
    remaining: Arc<RwLock<usize>>,
}

impl MalformedArgsProvider {
    fn new() -> Self {
        Self {
            remaining: Arc::new(RwLock::new(2)), // respond twice: malformed call + final message
        }
    }
}

impl Provider for MalformedArgsProvider {
    fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
        let mut remaining = self.remaining.write().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            if *remaining == 0 {
                Ok(ProviderResponse::Message(Message::assistant(
                    "failed to process malformed args",
                )))
            } else {
                Ok(ProviderResponse::ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    args: serde_json::json!({"path": 123}), // invalid: number instead of string
                })
            }
        } else {
            Err(Error::Provider("exhausted".into()))
        }
    }
}

#[test]
fn test_agent_returns_final_response() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "Hello, how can I help?",
    ))];
    let provider = pi_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "say hello");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Hello, how can I help?");
}

#[test]
fn test_agent_executes_tool_and_continues() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "echo hello"}),
        },
        ProviderResponse::Message(Message::assistant("Command executed successfully")),
    ];
    let provider = pi_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "run a command");
    assert!(result.is_ok());
    assert!(result.unwrap().contains("Command executed successfully"));
}

#[test]
fn test_agent_reads_file() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "test.txt"}),
        },
        ProviderResponse::Message(Message::assistant("File contains: hello world")),
    ];
    let provider = pi_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read the file");
    assert!(result.is_ok());
    assert!(result.unwrap().contains("hello world"));
}

#[test]
fn test_agent_transient_provider_failure_retries_then_succeeds() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "Success after retry",
    ))];
    let provider = TransientFailProvider::new(2, responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "test");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Success after retry");
}

#[test]
fn test_agent_repeated_provider_failure_stops_after_max_retries() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "should not reach",
    ))];
    let provider = TransientFailProvider::new(10, responses); // fails 10 times, max_retries is 3
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "test");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("repeatedly") || err.contains("retries"),
        "expected retry error: {}",
        err
    );
}

#[test]
fn test_agent_empty_response_retries_then_succeeds() {
    let (ctx, _temp) = make_test_context();
    let fallback = ProviderResponse::Message(Message::assistant("success after empty"));
    let provider = EmptyResponseProvider::new(2, fallback);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "test");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "success after empty");
}

#[test]
fn test_agent_max_steps_stops_runaway_loop() {
    let (ctx, _temp) = make_test_context();
    let provider = RunawayProvider;
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "run forever");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max steps"),
        "expected max steps error: {}",
        err
    );
}

#[test]
fn test_agent_malformed_tool_args_produces_clear_error() {
    let (ctx, _temp) = make_test_context();
    let provider = MalformedArgsProvider::new();
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "try to read");
    assert!(result.is_ok()); // agent should handle the error gracefully and continue
    assert!(result.unwrap().contains("failed to process"));
}

#[test]
fn test_agent_unknown_tool_produces_error_message() {
    let (ctx, _temp) = make_test_context();
    struct UnknownToolProvider {
        remaining: Arc<RwLock<usize>>,
    }
    impl UnknownToolProvider {
        fn new() -> Self {
            Self {
                remaining: Arc::new(RwLock::new(2)),
            }
        }
    }
    impl Provider for UnknownToolProvider {
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
            let mut remaining = self.remaining.write().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                if *remaining == 0 {
                    Ok(ProviderResponse::Message(Message::assistant(
                        "unknown tool handled gracefully",
                    )))
                } else {
                    Ok(ProviderResponse::ToolCall {
                        id: "1".into(),
                        name: "nonexistent_tool".into(),
                        args: serde_json::json!({"foo": "bar"}),
                    })
                }
            } else {
                Err(Error::Provider("exhausted".into()))
            }
        }
    }
    let provider = UnknownToolProvider::new();
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "try unknown tool");
    assert!(result.is_ok()); // should handle gracefully and return error in conversation
    assert!(result.unwrap().contains("unknown tool handled gracefully"));
}

#[test]
fn test_agent_with_custom_max_steps() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::default().with_max_steps(2);
    let provider = RunawayProvider;
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "run forever");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max steps"),
        "expected max steps error: {}",
        err
    );
}

#[test]
fn test_runtime_options_defaults() {
    let options = RuntimeOptions::default();
    assert_eq!(options.max_steps, 50);
    assert_eq!(options.max_provider_retries, 3);
    assert_eq!(options.max_read_bytes, 64 * 1024);
    assert_eq!(options.max_bash_output_bytes, 64 * 1024);
    assert_eq!(options.provider_timeout_secs, 120);
}

#[test]
fn test_runtime_options_builder() {
    let options = RuntimeOptions::new()
        .with_max_steps(100)
        .with_max_provider_retries(5)
        .with_max_read_bytes(32 * 1024)
        .with_max_bash_output_bytes(128 * 1024)
        .with_provider_timeout_secs(60);
    assert_eq!(options.max_steps, 100);
    assert_eq!(options.max_provider_retries, 5);
    assert_eq!(options.max_read_bytes, 32 * 1024);
    assert_eq!(options.max_bash_output_bytes, 128 * 1024);
    assert_eq!(options.provider_timeout_secs, 60);
}

use std::sync::{Arc, RwLock};
use tempfile::TempDir;
use topagent_core::{
    context::ExecutionContext,
    tools::{BashTool, EditTool, GitDiffTool, ReadTool, Tool, WriteTool},
    Agent, CancellationToken, Content, Error, ExecutionStage, Message, ProgressKind,
    ProgressUpdate, Provider, ProviderResponse, Role, RuntimeOptions, TaskResult, ToolCallEntry,
};

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

fn make_tools_with_git() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadTool::new()) as Box<dyn Tool>,
        Box::new(WriteTool::new()) as Box<dyn Tool>,
        Box::new(EditTool::new()) as Box<dyn Tool>,
        Box::new(BashTool::new()) as Box<dyn Tool>,
        Box::new(GitDiffTool::new()) as Box<dyn Tool>,
    ]
}

fn write_workspace_external_tools(temp: &TempDir, contents: &str) {
    let topagent_dir = temp.path().join(".topagent");
    std::fs::create_dir_all(&topagent_dir).unwrap();
    std::fs::write(topagent_dir.join("external-tools.json"), contents).unwrap();
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
    fn complete(
        &self,
        _messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> topagent_core::Result<ProviderResponse> {
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
    fn complete(
        &self,
        _messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> topagent_core::Result<ProviderResponse> {
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
    fn complete(
        &self,
        _messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> topagent_core::Result<ProviderResponse> {
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
    fn complete(
        &self,
        _messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> topagent_core::Result<ProviderResponse> {
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

fn capture_progress_updates() -> (
    Arc<std::sync::Mutex<Vec<ProgressUpdate>>>,
    topagent_core::ProgressCallback,
) {
    let updates = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = updates.clone();
    let callback: topagent_core::ProgressCallback = Arc::new(move |update| {
        sink.lock().unwrap().push(update);
    });
    (updates, callback)
}

#[test]
fn test_agent_returns_final_response() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "Hello, how can I help?",
    ))];
    let provider = topagent_core::ScriptedProvider::new(responses);
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
    let provider = topagent_core::ScriptedProvider::new(responses);
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
    let provider = topagent_core::ScriptedProvider::new(responses);
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
fn test_agent_surfaces_progress_for_tool_activity_and_completion() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "echo hello"}),
        },
        ProviderResponse::Message(Message::assistant("done")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "inspect the repository");
    assert!(result.is_ok());

    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Received));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Waiting for model response")));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Running tool: bash")));
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Completed));
}

#[test]
fn test_agent_surfaces_retry_progress() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "Success after retry",
    ))];
    let provider = TransientFailProvider::new(1, responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "test retries");
    assert!(result.is_ok());

    let updates = updates.lock().unwrap();
    assert!(updates
        .iter()
        .any(|u| u.kind == ProgressKind::Retrying && u.message.contains("retrying (1/3)")));
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Completed));
}

#[test]
fn test_agent_surfaces_blocked_progress_when_planning_is_required() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "touch blocked.txt"}),
        },
        ProviderResponse::ToolCall {
            id: "2".into(),
            name: "update_plan".into(),
            args: serde_json::json!({"items": [{"content": "Create blocked.txt", "status": "pending"}]}),
        },
        ProviderResponse::Message(Message::assistant("blocked")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "refactor the entire codebase");
    assert!(result.is_ok());

    let updates = updates.lock().unwrap();
    assert!(updates
        .iter()
        .any(|u| u.kind == ProgressKind::Blocked && u.message.contains("planning required")));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Planning next steps")));
}

#[test]
fn test_agent_surfaces_failed_progress_on_terminal_error() {
    let (ctx, _temp) = make_test_context();
    let provider = TransientFailProvider::new(
        10,
        vec![ProviderResponse::Message(Message::assistant(
            "should not reach",
        ))],
    );
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "test repeated failure");
    assert!(result.is_err());

    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Failed));
}

#[test]
fn test_agent_returns_stopped_when_cancelled_before_run() {
    let (ctx, _temp) = make_test_context();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ctx.with_cancel_token(cancel);
    let provider = topagent_core::ScriptedProvider::new(vec![ProviderResponse::Message(
        Message::assistant("should not run"),
    )]);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "stop now");
    assert!(matches!(result, Err(Error::Stopped(_))));

    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Stopped));
}

#[test]
fn test_agent_can_be_cancelled_during_bash_execution() {
    let (ctx, _temp) = make_test_context();
    let cancel = CancellationToken::new();
    let ctx = ctx.with_cancel_token(cancel.clone());
    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "sleep 5"}),
        },
        ProviderResponse::Message(Message::assistant("done")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.cancel();
    });

    let result = agent.run(&ctx, "run a long command");
    canceller.join().unwrap();

    assert!(matches!(result, Err(Error::Stopped(_))));
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
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
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

#[test]
fn test_agent_loads_external_tools_from_workspace_state_dir() {
    let temp = TempDir::new().unwrap();
    write_workspace_external_tools(
        &temp,
        r#"[
            {"name": "greet", "description": "Say hello", "command": "echo", "args_template": "hello {name}"},
            {"name": "version", "description": "Get version", "command": "echo", "args_template": "v1.0.0"}
        ]"#,
    );

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());
    let result = agent.run(&ctx, "test");

    assert!(result.is_ok());
    let external = agent.external_tools();
    assert!(external.get("greet").is_some());
    assert!(external.get("version").is_some());
}

#[test]
fn test_agent_external_tools_file_missing_is_ok() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());
    let result = agent.run(&ctx, "test");

    assert!(result.is_ok());
    let external = agent.external_tools();
    assert!(external.is_empty());
}

#[test]
fn test_repeated_runs_do_not_duplicate_genesis_tools() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());

    let specs_first_run = agent.tool_specs();

    agent.run(&ctx, "first run").unwrap();
    let specs_second_run = agent.tool_specs();

    agent.run(&ctx, "second run").unwrap();
    let specs_third_run = agent.tool_specs();

    assert_eq!(
        specs_first_run.len(),
        specs_second_run.len(),
        "tool count should not change between runs"
    );
    assert_eq!(
        specs_first_run.len(),
        specs_third_run.len(),
        "tool count should not change after third run"
    );

    let tool_names: Vec<&str> = specs_first_run.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"create_tool"),
        "create_tool should be registered"
    );
    assert!(
        tool_names.contains(&"repair_tool"),
        "repair_tool should be registered"
    );
    assert!(
        tool_names.contains(&"list_generated_tools"),
        "list_generated_tools should be registered"
    );
}

#[test]
fn test_genesis_tools_become_external_after_verification() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::create_dir_all(root.join(".topagent/tools/my_tool")).unwrap();
    std::fs::write(root.join(".topagent/tools/my_tool/script.sh"), "echo hello").unwrap();
    std::fs::write(
        root.join(".topagent/tools/my_tool/manifest.json"),
        serde_json::json!({
            "name": "my_tool",
            "description": "a verified tool",
            "command": "echo hello",
            "verified": true,
            "inputs": [],
            "argv_template": [],
            "manifest_version": 1
        })
        .to_string(),
    )
    .unwrap();

    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());
    agent.run(&ctx, "test").unwrap();

    let external = agent.external_tools();
    assert!(
        external.get("my_tool").is_some(),
        "verified generated tool should be loaded as external tool"
    );
}

#[test]
fn test_agent_legacy_commands_json_is_still_loaded() {
    let temp = TempDir::new().unwrap();
    std::fs::write(
        temp.path().join("commands.json"),
        r#"[{"name": "legacy_tool", "description": "Legacy tool", "command": "echo", "argv_template": ["hello"]}]"#,
    )
    .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());
    let result = agent.run(&ctx, "test");

    assert!(result.is_ok());
    assert!(agent.external_tools().get("legacy_tool").is_some());
}

#[test]
fn test_agent_external_tools_file_invalid_fails() {
    let temp = TempDir::new().unwrap();
    write_workspace_external_tools(&temp, "invalid json {");

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let mut agent = Agent::new(Box::new(TestProvider), make_tools());
    let result = agent.run(&ctx, "test");

    assert!(result.is_err());
}

#[test]
fn test_agent_multiple_tool_calls_execute_sequentially() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("a.txt").unwrap(), "content A").unwrap();
    std::fs::write(ctx.resolve_path("b.txt").unwrap(), "content B").unwrap();

    let responses = vec![
        ProviderResponse::ToolCalls(vec![
            ToolCallEntry {
                id: "call_1".into(),
                name: "read".into(),
                args: serde_json::json!({"path": "a.txt"}),
            },
            ToolCallEntry {
                id: "call_2".into(),
                name: "read".into(),
                args: serde_json::json!({"path": "b.txt"}),
            },
        ]),
        ProviderResponse::Message(Message::assistant("done")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read two files");
    assert!(result.is_ok());
}

#[test]
fn test_agent_second_tool_failure_continues_batch() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("a.txt").unwrap(), "content A").unwrap();
    std::fs::write(ctx.resolve_path("b.txt").unwrap(), "content B").unwrap();

    let responses = vec![
        ProviderResponse::ToolCalls(vec![
            ToolCallEntry {
                id: "call_1".into(),
                name: "read".into(),
                args: serde_json::json!({"path": "a.txt"}),
            },
            ToolCallEntry {
                id: "call_2".into(),
                name: "nonexistent_tool".into(),
                args: serde_json::json!({"foo": "bar"}),
            },
            ToolCallEntry {
                id: "call_3".into(),
                name: "read".into(),
                args: serde_json::json!({"path": "b.txt"}),
            },
        ]),
        ProviderResponse::Message(Message::assistant("done")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "call three tools");
    assert!(
        result.is_ok(),
        "expected run to succeed despite tool failure: {:?}",
        result
    );
}

#[test]
fn test_agent_multi_tool_batch_counts_steps() {
    let (ctx, _temp) = make_test_context();

    struct MultiToolProvider;
    impl Provider for MultiToolProvider {
        fn complete(
            &self,
            _messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            Ok(ProviderResponse::ToolCalls(vec![
                ToolCallEntry {
                    id: "1".into(),
                    name: "bash".into(),
                    args: serde_json::json!({"command": "echo a"}),
                },
                ToolCallEntry {
                    id: "2".into(),
                    name: "bash".into(),
                    args: serde_json::json!({"command": "echo b"}),
                },
            ]))
        }
    }

    let options = RuntimeOptions::default().with_max_steps(1);
    let provider = MultiToolProvider;
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "run two bash commands");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max steps"),
        "expected max steps error: {}",
        err
    );
}

#[test]
fn test_no_pi_md_includes_absence_note() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct CheckPromptProvider {
        pub captured_messages: Arc<RwLock<Vec<Message>>>,
    }
    impl CheckPromptProvider {
        fn new() -> Self {
            Self {
                captured_messages: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }
    impl Provider for CheckPromptProvider {
        fn complete(
            &self,
            messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            let mut captured = self.captured_messages.write().unwrap();
            captured.extend(messages.to_vec());
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }
    let provider = CheckPromptProvider::new();
    let provider_ref = Arc::clone(&provider.captured_messages);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let _ = agent.run(&ctx, "test");
    let captured = provider_ref.read().unwrap();
    let system_prompt = captured.first().and_then(|m| m.as_text()).unwrap_or("");
    assert!(
        system_prompt.contains("No TOPAGENT.md file is present"),
        "expected TOPAGENT.md absence note in system prompt: {}",
        system_prompt
    );
}

#[test]
fn test_topagent_md_loaded_when_present() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(
        root.join("TOPAGENT.md"),
        "# Custom Instructions\nUse Rust.\n",
    )
    .unwrap();
    let ctx = ExecutionContext::new(root);

    struct CheckPromptProvider {
        pub captured_messages: Arc<RwLock<Vec<Message>>>,
    }
    impl CheckPromptProvider {
        fn new() -> Self {
            Self {
                captured_messages: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }
    impl Provider for CheckPromptProvider {
        fn complete(
            &self,
            messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            let mut captured = self.captured_messages.write().unwrap();
            captured.extend(messages.to_vec());
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }
    let provider = CheckPromptProvider::new();
    let provider_ref = Arc::clone(&provider.captured_messages);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let _ = agent.run(&ctx, "test");
    let captured = provider_ref.read().unwrap();
    let system_prompt = captured.first().and_then(|m| m.as_text()).unwrap_or("");
    assert!(
        system_prompt.contains("Custom Instructions"),
        "expected TOPAGENT.md content in system prompt: {}",
        system_prompt
    );
    assert!(
        !system_prompt.contains("No TOPAGENT.md file is present"),
        "should not have absence note when TOPAGENT.md exists: {}",
        system_prompt
    );
}

#[test]
fn test_workspace_memory_context_is_included_in_system_prompt() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root).with_memory_context(
        "Treat memory as hints, not truth.\n- [verified] architecture -> topics/architecture.md",
    );

    struct CheckPromptProvider {
        pub captured_messages: Arc<RwLock<Vec<Message>>>,
    }
    impl CheckPromptProvider {
        fn new() -> Self {
            Self {
                captured_messages: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }
    impl Provider for CheckPromptProvider {
        fn complete(
            &self,
            messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            let mut captured = self.captured_messages.write().unwrap();
            captured.extend(messages.to_vec());
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }

    let provider = CheckPromptProvider::new();
    let provider_ref = Arc::clone(&provider.captured_messages);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let _ = agent.run(&ctx, "test");
    let captured = provider_ref.read().unwrap();
    let system_prompt = captured.first().and_then(|m| m.as_text()).unwrap_or("");

    assert!(
        system_prompt.contains("Workspace Memory"),
        "expected workspace memory section in system prompt: {}",
        system_prompt
    );
    assert!(
        system_prompt.contains("Treat memory as hints, not truth"),
        "expected memory skepticism note in system prompt: {}",
        system_prompt
    );
}

#[test]
fn test_agent_tracks_changed_files_on_write() {
    let (ctx, _temp) = make_test_context();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "new_file.txt", "content": "hello world"}),
        },
        ProviderResponse::Message(Message::assistant("file written")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "write a file");
    assert!(result.is_ok());
    assert_eq!(agent.changed_files(), &["new_file.txt"]);
}

#[test]
fn test_agent_tracks_changed_files_on_edit() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(
        ctx.resolve_path("existing.txt").unwrap(),
        "original content",
    )
    .unwrap();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "existing.txt", "old_text": "original", "new_text": "modified"}),
        },
        ProviderResponse::Message(Message::assistant("file edited")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "edit a file");
    assert!(result.is_ok());
    assert_eq!(agent.changed_files(), vec!["existing.txt"]);
}

#[test]
fn test_agent_tracks_multiple_changed_files() {
    let (ctx, _temp) = make_test_context();

    let responses = vec![
        ProviderResponse::ToolCalls(vec![
            ToolCallEntry {
                id: "1".into(),
                name: "write".into(),
                args: serde_json::json!({"path": "file1.txt", "content": "content 1"}),
            },
            ToolCallEntry {
                id: "2".into(),
                name: "write".into(),
                args: serde_json::json!({"path": "file2.txt", "content": "content 2"}),
            },
        ]),
        ProviderResponse::Message(Message::assistant("files written")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "write two files");
    assert!(result.is_ok());
    let changed = agent.changed_files();
    assert!(changed.contains(&"file1.txt".to_string()));
    assert!(changed.contains(&"file2.txt".to_string()));
    assert_eq!(changed.len(), 2);
}

#[test]
fn test_agent_tracks_changed_files_after_failed_write() {
    let (ctx, _temp) = make_test_context();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "bad|path/file.txt", "content": "should fail"}),
        },
        ProviderResponse::Message(Message::assistant("write failed")),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "try to write to invalid path");
    assert!(result.is_ok());
}

#[test]
fn test_git_diff_shows_actual_content() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("test.txt"), "original").unwrap();
    std::process::Command::new("git")
        .args(["add", "test.txt"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("test.txt"), "modified content").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let mut agent = Agent::new(
        Box::new(topagent_core::ScriptedProvider::new(vec![
            ProviderResponse::ToolCall {
                id: "1".into(),
                name: "git_diff".into(),
                args: serde_json::json!({}),
            },
            ProviderResponse::Message(Message::assistant("here is the diff")),
        ])),
        make_tools_with_git(),
    );
    let result = agent.run(&ctx, "show diff");
    if let Err(e) = &result {
        eprintln!("agent.run() failed: {}", e);
    }
    assert!(result.is_ok(), "agent.run() failed: {:?}", result.err());
    let output = result.unwrap();
    assert!(
        output.contains("diff") || output.contains("modified"),
        "expected git diff to show diff output, got: {}",
        output
    );
}

#[test]
fn test_verification_section_in_system_prompt() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct CheckPromptProvider {
        pub captured_messages: Arc<RwLock<Vec<Message>>>,
    }
    impl CheckPromptProvider {
        fn new() -> Self {
            Self {
                captured_messages: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }
    impl Provider for CheckPromptProvider {
        fn complete(
            &self,
            messages: &[Message],
            _route: &topagent_core::ModelRoute,
        ) -> topagent_core::Result<ProviderResponse> {
            let mut captured = self.captured_messages.write().unwrap();
            captured.extend(messages.to_vec());
            Ok(ProviderResponse::Message(Message::assistant("done")))
        }
    }
    let provider = CheckPromptProvider::new();
    let provider_ref = Arc::clone(&provider.captured_messages);
    let mut agent = Agent::new(Box::new(provider), make_tools());
    let _ = agent.run(&ctx, "test");
    let captured = provider_ref.read().unwrap();
    let system_prompt = captured.first().and_then(|m| m.as_text()).unwrap_or("");
    assert!(
        system_prompt.contains("## Verification") || system_prompt.contains("git_diff"),
        "expected verification section or git_diff mention in system prompt: {}",
        system_prompt
    );
}

#[test]
fn test_runtime_options_defaults_include_require_plan() {
    let options = RuntimeOptions::default();
    assert!(options.require_plan, "require_plan should default to true");
}

#[test]
fn test_runtime_options_builder_with_require_plan() {
    let options = RuntimeOptions::new().with_require_plan(false);
    assert!(!options.require_plan);
}

#[test]
fn test_task_result_format_no_evidence() {
    let result = TaskResult::new("Task completed".to_string());
    let formatted = result.format_proof_of_work();
    assert_eq!(formatted, "Task completed");
}

#[test]
fn test_task_result_format_with_files_changed() {
    let result =
        TaskResult::new("Edit done".to_string()).with_files_changed(vec!["src/lib.rs".to_string()]);
    let formatted = result.format_proof_of_work();
    assert!(formatted.contains("src/lib.rs"));
    assert!(formatted.contains("Files Changed"));
}

#[test]
fn test_task_result_format_with_verification_commands() {
    use topagent_core::task_result::VerificationCommand;
    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "test result: ok".to_string(),
        exit_code: 0,
        succeeded: true,
    };
    let result = TaskResult::new("Tests passed".to_string()).with_verification_command(cmd);
    let formatted = result.format_proof_of_work();
    assert!(formatted.contains("Verification"));
    assert!(formatted.contains("PASS"));
}

#[test]
fn test_task_result_format_with_failed_verification() {
    use topagent_core::task_result::VerificationCommand;
    let cmd = VerificationCommand {
        command: "cargo build".to_string(),
        output: "error: build failed".to_string(),
        exit_code: 1,
        succeeded: false,
    };
    let result = TaskResult::new("Build failed".to_string()).with_verification_command(cmd);
    let formatted = result.format_proof_of_work();
    assert!(formatted.contains("FAIL"));
    assert!(formatted.contains("error: build failed"));
}

#[test]
fn test_planning_gate_blocks_mutation_tool() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "test.txt", "content": "hello"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "First step", "status": "done"}]}),
        },
        ProviderResponse::Message(Message::assistant("Plan created".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase and then test it");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Plan created"));
}

#[test]
fn test_planning_gate_allows_plan_tool() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "First step", "status": "done"}]}),
        },
        ProviderResponse::Message(Message::assistant("Plan created".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase and then test it");
    assert!(result.is_ok());
}

#[test]
fn test_planning_gate_allows_read_tool() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "test.txt"}),
        },
        ProviderResponse::Message(Message::assistant("File contents".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase and then test it");
    assert!(result.is_ok());
}

#[test]
fn test_trivial_task_not_blocked() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![ProviderResponse::Message(Message::assistant(
        "Done".to_string(),
    ))]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "read this file");
    assert!(result.is_ok());
}

#[test]
fn test_small_scoped_mutation_with_verification_request_is_not_forced_to_plan() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "README.md", "content": "updated"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "fix the typo in README.md and run tests");

    assert!(result.is_ok());
    assert_eq!(agent.changed_files(), vec!["README.md"]);
    assert!(
        !agent.is_planning_gate_active(),
        "small scoped mutation should not leave planning gate active"
    );
}

#[test]
fn test_non_trivial_task_can_plan_mutate_verify_and_complete() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("README.md").unwrap(), "original").unwrap();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "README.md"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({
                "items": [
                    {"content": "Inspect README.md", "status": "done"},
                    {"content": "Update README.md", "status": "in_progress"},
                    {"content": "Verify the change", "status": "pending"}
                ]
            }),
        },
        ProviderResponse::ToolCall {
            id: "3".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "README.md", "content": "updated"}),
        },
        ProviderResponse::ToolCall {
            id: "4".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test --help >/dev/null 2>&1"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("README.md"),
        "proof-of-work should include the changed file: {}",
        output
    );
    assert!(
        output.contains("Verification"),
        "proof-of-work should include verification evidence: {}",
        output
    );
    assert!(
        !agent.is_planning_gate_active(),
        "planning gate should be cleared after a real plan is created"
    );
    let updates = updates.lock().unwrap();
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Running tool: read")));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Planning next steps")));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Running tool: write")));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Running tool: bash (verification)")));
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Completed));
    assert!(!updates.iter().any(|u| u.kind == ProgressKind::Failed));
}

#[test]
fn test_safe_bash_allowed_before_plan() {
    use topagent_core::Agent;
    use topagent_core::BashCommandClass;

    assert_eq!(
        Agent::classify_bash_command("ls -la"),
        BashCommandClass::ResearchSafe
    );
    assert_eq!(
        Agent::classify_bash_command("git status"),
        BashCommandClass::ResearchSafe
    );
    assert_eq!(
        Agent::classify_bash_command("rg 'fn main' src/"),
        BashCommandClass::ResearchSafe
    );
    assert_eq!(
        Agent::classify_bash_command("find . -name '*.rs'"),
        BashCommandClass::ResearchSafe
    );
}

#[test]
fn test_unsafe_bash_blocked_before_plan() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "rm -rf src"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "Step 1", "status": "done"}]}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase");
    assert!(result.is_ok());
}

#[test]
fn test_verification_bash_allowed_before_plan() {
    use topagent_core::Agent;
    use topagent_core::BashCommandClass;

    assert_eq!(
        Agent::classify_bash_command("cargo test"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("cargo build"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("cargo clippy"),
        BashCommandClass::Verification
    );
}

#[test]
fn test_verification_bash_with_flags_allowed_before_plan() {
    use topagent_core::Agent;
    use topagent_core::BashCommandClass;

    assert_eq!(
        Agent::classify_bash_command("cargo test --lib"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("cargo build --release"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("cargo test -- --test-threads=1"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("npm test -- --coverage"),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("go test -v ./..."),
        BashCommandClass::Verification
    );
    assert_eq!(
        Agent::classify_bash_command("make verify"),
        BashCommandClass::Verification
    );
}

#[test]
fn test_unknown_bash_blocked_before_plan() {
    use topagent_core::Agent;
    use topagent_core::BashCommandClass;

    assert_eq!(
        Agent::classify_bash_command("some_unknown_command"),
        BashCommandClass::MutationRisk
    );
}

#[test]
fn test_unsafe_bash_allowed_after_plan_exists() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "Step 1", "status": "done"}]}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "rm -rf src"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Done"));
}

#[test]
fn test_repeated_planning_blocks_auto_create_plan_and_recover() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    // Writes 1-5 are blocked (the 5th triggers generate_or_fallback_plan).
    // That call consumes one response for the LLM plan-generation attempt.
    // Write 7 succeeds because the gate is now open. Then the text response ends.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked-1.txt", "content": "one"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked-2.txt", "content": "two"}),
        },
        ProviderResponse::ToolCall {
            id: "3".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked-3.txt", "content": "three"}),
        },
        ProviderResponse::ToolCall {
            id: "4".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked-4.txt", "content": "four"}),
        },
        ProviderResponse::ToolCall {
            id: "5".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked-5.txt", "content": "five"}),
        },
        // Consumed by try_generate_plan LLM call inside generate_or_fallback_plan:
        ProviderResponse::Message(Message::assistant(
            "1. Execute the requested changes\n2. Verify".to_string(),
        )),
        ProviderResponse::ToolCall {
            id: "7".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "recovered.txt", "content": "recovered"}),
        },
        ProviderResponse::Message(Message::assistant(
            "Auto-plan recovered the task.".to_string(),
        )),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    // With auto-plan, the task recovers instead of failing.
    assert!(result.is_ok());
    assert!(ctx.workspace_root.join("recovered.txt").exists());
}

#[test]
fn test_blocked_write_then_text_response_redirected_to_plan() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    // Write is blocked (1 block). Text response is redirected (1 redirect).
    // Second text hits MAX_PLANNING_REDIRECTS, triggers generate_or_fallback_plan
    // which consumes one response for the LLM plan generation attempt.
    // Next text response accepted as final answer with plan in place.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked.txt", "content": "hello"}),
        },
        ProviderResponse::Message(Message::assistant(
            "I could not create a plan, but here is a summary.".to_string(),
        )),
        ProviderResponse::Message(Message::assistant("Fine, I still won't plan.".to_string())),
        // Consumed by try_generate_plan:
        ProviderResponse::Message(Message::assistant("1. Make changes\n2. Test".to_string())),
        ProviderResponse::Message(Message::assistant("Done with auto-plan.".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    // Text responses during planning phase are redirected, not accepted as final.
    // After MAX_PLANNING_REDIRECTS, a plan is generated and the next response completes.
    assert!(result.is_ok());
    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Blocked));
}

#[test]
fn test_blocked_then_plan_then_complete_has_single_final_completed_state() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked.txt", "content": "draft"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({
                "items": [
                    {"content": "Create blocked.txt", "status": "in_progress"},
                    {"content": "Verify the result", "status": "pending"}
                ]
            }),
        },
        ProviderResponse::ToolCall {
            id: "3".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "blocked.txt", "content": "final"}),
        },
        ProviderResponse::ToolCall {
            id: "4".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test --help >/dev/null 2>&1"}),
        },
        ProviderResponse::Message(Message::assistant("Recovered and finished".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    assert!(result.is_ok());
    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Blocked));
    assert!(updates
        .iter()
        .any(|u| u.message.contains("Planning next steps")));
    let terminal_updates: Vec<_> = updates.iter().filter(|u| u.is_terminal()).collect();
    assert_eq!(
        terminal_updates.len(),
        1,
        "expected one terminal status, got {:?}",
        terminal_updates
    );
    assert_eq!(terminal_updates[0].kind, ProgressKind::Completed);
    assert!(!updates.iter().any(|u| u.kind == ProgressKind::Failed));
    assert!(!updates.iter().any(|u| u.kind == ProgressKind::Stopped));
}

#[test]
fn test_external_tool_blocked_before_plan() {
    let temp = tempfile::TempDir::new().unwrap();
    write_workspace_external_tools(
        &temp,
        r#"[{"name": "my_tool", "description": "test", "command": "echo", "argv_template": ["hello"]}]"#,
    );
    std::fs::write(temp.path().join("test.txt"), "content").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "my_tool".to_string(),
            args: serde_json::json!({}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let result = agent.run(&ctx, "refactor the entire codebase");
    assert!(result.is_ok());
}

#[test]
fn test_read_only_task_no_file_change_evidence() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("test.txt").unwrap(), "hello").unwrap();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "test.txt"}),
        },
        ProviderResponse::Message(Message::assistant("File contains hello".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read the file");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("File contains hello"));
    assert!(!output.contains("Files Changed"));
}

struct BasicTestProvider {
    responses: Vec<ProviderResponse>,
    idx: Arc<RwLock<usize>>,
}

impl BasicTestProvider {
    fn new(responses: Vec<ProviderResponse>) -> Self {
        Self {
            responses,
            idx: Arc::new(RwLock::new(0)),
        }
    }
}

impl Provider for BasicTestProvider {
    fn complete(
        &self,
        _messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> Result<ProviderResponse, Error> {
        let mut idx = self.idx.write().unwrap();
        if *idx < self.responses.len() {
            let resp = self.responses[*idx].clone();
            *idx += 1;
            Ok(resp)
        } else {
            Ok(ProviderResponse::Message(Message::assistant(
                "Done".to_string(),
            )))
        }
    }
}

#[test]
fn test_bash_mutation_tracked_in_proof_of_work() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "echo 'hello' > new_file.txt"}),
        },
        ProviderResponse::Message(Message::assistant("File created via bash".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "create a file");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("new_file.txt"),
        "bash-created file should appear in proof of work: {}",
        output
    );
}

#[test]
fn test_external_tool_mutation_tracked() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create external tool that touches a file
    write_workspace_external_tools(
        &temp,
        r#"[{"name": "create_file", "description": "Create a file", "command": "touch", "argv_template": ["{filename}"]}]"#,
    );

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "create_file".into(),
            args: serde_json::json!({"filename": "external_created.txt"}),
        },
        ProviderResponse::Message(Message::assistant("External tool executed".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "create a file with external tool");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("external_created.txt"),
        "external-tool-created file should appear in proof of work: {}",
        output
    );
}

#[test]
fn test_read_only_task_no_fake_changes() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
        ProviderResponse::Message(Message::assistant("File contents shown".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read the file");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        !output.contains("Files Changed"),
        "read-only task should not show file changes: {}",
        output
    );
    assert!(
        !output.contains("README.md"),
        "read-only task should not list files: {}",
        output
    );
}

#[test]
fn test_preexisting_dirty_not_attributed_to_run() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    // Create pre-existing dirty file
    std::fs::write(temp.path().join("dirty.txt"), "pre-existing dirty content").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "ls -la"}),
        },
        ProviderResponse::Message(Message::assistant("Listed files".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "list files");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        !output.contains("dirty.txt"),
        "pre-existing dirty file should not appear in this run's proof of work: {}",
        output
    );
}

#[test]
fn test_execution_stage_transitions_to_edit() {
    let (ctx, _temp) = make_test_context();
    std::fs::write(ctx.resolve_path("test.txt").unwrap(), "original").unwrap();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "new.txt", "content": "new content"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "write a file");
    assert!(result.is_ok());

    // Check that execution stage moved to Edit
    assert_eq!(
        agent.execution_stage(),
        ExecutionStage::Edit,
        "execution stage should be Edit after write operation"
    );
}

#[test]
fn test_planning_gate_cleared_after_plan_creation() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "Step 1", "status": "pending"}]}),
        },
        ProviderResponse::Message(Message::assistant("Plan created".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");
    assert!(result.is_ok());

    // Planning gate should be cleared after plan is created
    assert!(
        !agent.is_planning_gate_active(),
        "planning gate should be cleared after plan creation"
    );
}

#[test]
fn test_route_selection_follows_execution_stage() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new()
        .with_research_model("research-model".to_string())
        .with_edit_model("edit-model".to_string())
        .with_review_model("review-model".to_string());

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "test.txt", "content": "content"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "write a file");
    assert!(result.is_ok());

    // After write operation, route should use edit model
    let route = agent.get_route();
    assert_eq!(
        route.model_id, "edit-model",
        "route should use edit model after entering edit stage"
    );
}

#[test]
fn test_preexisting_dirty_file_changed_during_run() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("dirty.txt"), "original content").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "dirty.txt", "content": "modified content"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "modify dirty file");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("dirty.txt"),
        "pre-existing dirty file that was actually modified should appear in proof of work: {}",
        output
    );
    assert!(
        output.contains("pre-existing dirty"),
        "should label as pre-existing dirty: {}",
        output
    );
}

#[test]
fn test_preexisting_dirty_file_unchanged_not_reported() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("dirty.txt"), "pre-existing content").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "dirty.txt"}),
        },
        ProviderResponse::Message(Message::assistant("Read file".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read dirty file");
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        !output.contains("dirty.txt"),
        "pre-existing dirty file that was NOT modified should NOT appear in proof of work: {}",
        output
    );
}

#[cfg(unix)]
#[test]
fn test_preexisting_dirty_file_missing_baseline_is_labeled_uncertain() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let dirty_path = temp.path().join("dirty.txt");
    std::fs::write(&dirty_path, "pre-existing content").unwrap();
    let mut permissions = std::fs::metadata(&dirty_path).unwrap().permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&dirty_path, permissions).unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
        ProviderResponse::Message(Message::assistant("Read file".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "read file");

    let mut restore_permissions = std::fs::metadata(&dirty_path).unwrap().permissions();
    restore_permissions.set_mode(0o644);
    std::fs::set_permissions(&dirty_path, restore_permissions).unwrap();

    assert!(result.is_ok());
    assert!(
        agent.changed_files().is_empty(),
        "missing-baseline dirty file should not be credited as changed"
    );

    let output = result.unwrap();
    assert!(
        !output.contains("dirty.txt (pre-existing dirty, changed again during this run)"),
        "missing-baseline dirty file should not be reported as changed during this run: {}",
        output
    );
    assert!(
        output.contains("dirty.txt"),
        "missing-baseline dirty file should be surfaced as uncertain when reported: {}",
        output
    );
    assert!(
        output.contains("baseline unavailable, run attribution uncertain"),
        "missing-baseline dirty file should be labeled uncertain: {}",
        output
    );
}

#[test]
fn test_write_without_verification_reports_gap() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "new.txt", "content": "new content"}),
        },
        ProviderResponse::Message(Message::assistant("Done".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "write a file");
    assert!(result.is_ok());

    let output = result.unwrap();
    assert!(
        output.contains("new.txt"),
        "new file should appear in proof of work: {}",
        output
    );
    assert!(
        output.contains("Files were modified but no verification commands were run"),
        "missing verification should be called out explicitly: {}",
        output
    );
}

#[test]
fn test_verification_bash_does_not_force_edit_stage() {
    let (ctx, _temp) = make_test_context();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "cargo test"}),
        },
        ProviderResponse::Message(Message::assistant("Tests passed".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "run tests");
    assert!(result.is_ok());

    assert_eq!(
        agent.execution_stage(),
        ExecutionStage::Research,
        "verification bash should NOT force Edit stage when no files are actually mutated"
    );
}

#[test]
fn test_verification_bash_mutation_is_tracked_and_switches_stage() {
    let temp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    std::fs::write(temp.path().join("README.md"), "# Initial").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({
                "command": "cargo test >/dev/null 2>&1 || true; echo 'mutated' > changed_by_verification.txt; false"
            }),
        },
        ProviderResponse::Message(Message::assistant("Verification finished".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "run verification");
    assert!(result.is_ok());

    assert!(
        agent
            .changed_files()
            .contains(&"changed_by_verification.txt".to_string()),
        "verification-classified bash should record real mutation"
    );
    assert_eq!(
        agent.execution_stage(),
        ExecutionStage::Edit,
        "verification-classified bash should switch to Edit when files actually change"
    );

    let output = result.unwrap();
    assert!(
        output.contains("changed_by_verification.txt"),
        "verification-created file should appear in proof of work: {}",
        output
    );
    assert!(
        output.contains("### Verification"),
        "verification evidence should still be included: {}",
        output
    );
}

#[test]
fn test_readonly_bash_stays_in_research() {
    let (ctx, _temp) = make_test_context();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "ls -la"}),
        },
        ProviderResponse::Message(Message::assistant("Listed".to_string())),
    ];
    let provider = topagent_core::ScriptedProvider::new(responses);
    let mut agent = Agent::new(Box::new(provider), make_tools());

    let result = agent.run(&ctx, "list files");
    assert!(result.is_ok());

    assert_eq!(
        agent.execution_stage(),
        ExecutionStage::Research,
        "read-only bash should stay in Research stage"
    );
}

#[test]
fn test_planning_phase_budget_triggers_auto_plan_after_research_loop() {
    let (ctx, temp) = make_test_context();
    std::fs::write(temp.path().join("file.txt"), "content").unwrap();
    let options = RuntimeOptions::new()
        .with_require_plan(true)
        .with_max_steps(20);

    // Simulate model doing pure research reads without ever calling update_plan.
    // planning_phase_steps increments at the top of each iteration (before the
    // provider call), so after 9 iterations consuming reads r0–r8 the counter
    // reaches 10 on iteration 10. generate_or_fallback_plan fires and
    // try_generate_plan consumes the next queued response (the plan message).
    // The main loop's provider call then consumes the following response.
    let mut responses: Vec<ProviderResponse> = (0..9)
        .map(|i| ProviderResponse::ToolCall {
            id: format!("r{}", i),
            name: "read".to_string(),
            args: serde_json::json!({"path": "file.txt"}),
        })
        .collect();
    // Consumed by try_generate_plan when planning_phase_steps reaches 10:
    responses.push(ProviderResponse::Message(Message::assistant(
        "1. Read relevant files\n2. Make changes\n3. Test".to_string(),
    )));
    responses.push(ProviderResponse::ToolCall {
        id: "w1".to_string(),
        name: "write".to_string(),
        args: serde_json::json!({"path": "output.txt", "content": "done"}),
    });
    responses.push(ProviderResponse::Message(Message::assistant(
        "Completed.".to_string(),
    )));

    let provider = BasicTestProvider::new(responses);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    assert!(
        result.is_ok(),
        "task should complete after auto-plan from phase budget"
    );
    assert!(
        temp.path().join("output.txt").exists(),
        "write should succeed after auto-plan"
    );
}

#[test]
fn test_text_response_during_planning_phase_is_redirected() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);

    // Model immediately tries to return a text answer without planning.
    // First text is redirected (redirect 1). Second text triggers
    // generate_or_fallback_plan (redirect 2 = MAX_PLANNING_REDIRECTS),
    // which consumes one response for LLM plan generation.
    // Next text accepted as final.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::Message(Message::assistant(
            "I'll just describe what to do.".to_string(),
        )),
        ProviderResponse::Message(Message::assistant("Still not planning.".to_string())),
        // Consumed by try_generate_plan:
        ProviderResponse::Message(Message::assistant(
            "1. Investigate\n2. Implement\n3. Test".to_string(),
        )),
        ProviderResponse::Message(Message::assistant(
            "Final answer after auto-plan.".to_string(),
        )),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let (_updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Final answer after auto-plan"));
    // Planning gate should be deactivated after auto-plan
    assert!(!agent.is_planning_gate_active());
}

#[test]
fn test_text_response_after_plan_creation_accepted_normally() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);

    // Model creates a plan, then returns a text response — should be accepted
    // as the final answer without redirect.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [{"content": "Step 1", "status": "pending"}]}),
        },
        ProviderResponse::Message(Message::assistant("Plan created and ready.".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");

    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Plan created and ready"));
}

#[test]
fn test_runtime_escalation_activates_planning_gate_after_multi_file_mutations() {
    let (ctx, _temp) = make_test_context();
    // require_plan is true but task is classified as direct (short instruction)
    let options = RuntimeOptions::new().with_require_plan(true);

    // Write 3 distinct files without a plan. The escalation threshold (3) should
    // activate the planning gate, blocking the 4th write. The generate_or_fallback_plan
    // call then consumes one response for LLM plan generation, and the agent recovers.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "a.txt", "content": "a"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "b.txt", "content": "b"}),
        },
        ProviderResponse::ToolCall {
            id: "3".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "c.txt", "content": "c"}),
        },
        // This write is blocked because escalation activated the gate
        ProviderResponse::ToolCall {
            id: "4".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "d.txt", "content": "d"}),
        },
        // Model creates a plan after being blocked
        ProviderResponse::ToolCall {
            id: "5".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({"items": [
                {"content": "Write files", "status": "in_progress"},
                {"content": "Verify", "status": "pending"}
            ]}),
        },
        // Now the write succeeds
        ProviderResponse::ToolCall {
            id: "6".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "d.txt", "content": "d"}),
        },
        ProviderResponse::Message(Message::assistant("Done.".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);
    let (updates, callback) = capture_progress_updates();
    agent.set_progress_callback(Some(callback));

    // Short instruction — classified as direct execution by heuristic
    let result = agent.run(&ctx, "write four files");

    assert!(result.is_ok());
    // The gate should have been escalated and then resolved
    assert!(!agent.is_planning_gate_active());
    // All files should exist
    assert!(ctx.workspace_root.join("a.txt").exists());
    assert!(ctx.workspace_root.join("b.txt").exists());
    assert!(ctx.workspace_root.join("c.txt").exists());
    assert!(ctx.workspace_root.join("d.txt").exists());
    // Should have seen a blocked progress update from escalation
    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|u| u.kind == ProgressKind::Blocked));
}

#[test]
fn test_llm_plan_generation_produces_concrete_plan() {
    let (ctx, _temp) = make_test_context();
    let options = RuntimeOptions::new().with_require_plan(true);

    // Model tries to write immediately (blocked), then tries again 4 more times
    // (5 total blocks triggers generate_or_fallback_plan). The LLM plan-generation
    // call returns a concrete numbered list which becomes the actual plan.
    let provider = BasicTestProvider::new(vec![
        ProviderResponse::ToolCall {
            id: "1".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "x.txt", "content": "x"}),
        },
        ProviderResponse::ToolCall {
            id: "2".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "x.txt", "content": "x"}),
        },
        ProviderResponse::ToolCall {
            id: "3".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "x.txt", "content": "x"}),
        },
        ProviderResponse::ToolCall {
            id: "4".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "x.txt", "content": "x"}),
        },
        ProviderResponse::ToolCall {
            id: "5".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "x.txt", "content": "x"}),
        },
        // Consumed by try_generate_plan — returns a real plan:
        ProviderResponse::Message(Message::assistant(
            "1. Read the config file\n2. Update the parser\n3. Add tests\n4. Run cargo test"
                .to_string(),
        )),
        ProviderResponse::Message(Message::assistant("Done.".to_string())),
    ]);
    let mut agent = Agent::with_options(Box::new(provider), make_tools(), options);

    let result = agent.run(&ctx, "refactor the entire codebase and then test it");
    assert!(result.is_ok());

    // Verify the plan has the LLM-generated steps, not the generic fallback
    let plan = agent.plan();
    let plan = plan.lock().unwrap();
    assert!(plan.has_items());
    let items: Vec<&str> = plan
        .items()
        .iter()
        .map(|i| i.description.as_str())
        .collect();
    assert!(
        items
            .iter()
            .any(|d| d.contains("config") || d.contains("parser")),
        "plan should contain LLM-generated steps, not generic fallback: {:?}",
        items
    );
}

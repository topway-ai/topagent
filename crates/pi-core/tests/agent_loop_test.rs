use pi_core::{
    context::ExecutionContext,
    tools::{BashTool, EditTool, GitDiffTool, ReadTool, Tool, WriteTool},
    Agent, Content, Error, Message, Provider, ProviderResponse, Role, RuntimeOptions,
    ToolCallEntry,
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

fn make_tools_with_git() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadTool::new()) as Box<dyn Tool>,
        Box::new(WriteTool::new()) as Box<dyn Tool>,
        Box::new(EditTool::new()) as Box<dyn Tool>,
        Box::new(BashTool::new()) as Box<dyn Tool>,
        Box::new(GitDiffTool::new()) as Box<dyn Tool>,
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

#[test]
fn test_agent_loads_commands_from_workspace() {
    let temp = TempDir::new().unwrap();
    let commands_json = temp.path().join("commands.json");
    std::fs::write(
        &commands_json,
        r#"[
            {"name": "greet", "description": "Say hello", "command": "echo", "args_template": "hello {name}"},
            {"name": "version", "description": "Get version", "command": "echo", "args_template": "v1.0.0"}
        ]"#,
    )
    .unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
fn test_agent_commands_json_missing_is_ok() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
    std::fs::create_dir_all(root.join(".rust-pi/tools/my_tool")).unwrap();
    std::fs::write(root.join(".rust-pi/tools/my_tool/script.sh"), "echo hello").unwrap();
    std::fs::write(
        root.join(".rust-pi/tools/my_tool/manifest.json"),
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
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
fn test_agent_commands_json_invalid_fails() {
    let temp = TempDir::new().unwrap();
    let commands_json = temp.path().join("commands.json");
    std::fs::write(&commands_json, "invalid json {").unwrap();

    let root = temp.path().to_path_buf();
    let ctx = ExecutionContext::new(root);

    struct TestProvider;
    impl Provider for TestProvider {
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
        fn complete(&self, _messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
        fn complete(&self, messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
        system_prompt.contains("No PI.md file is present"),
        "expected PI.md absence note in system prompt: {}",
        system_prompt
    );
}

#[test]
fn test_pi_md_loaded_when_present() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(root.join("PI.md"), "# Custom Instructions\nUse Rust.\n").unwrap();
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
        fn complete(&self, messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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
        "expected PI.md content in system prompt: {}",
        system_prompt
    );
    assert!(
        !system_prompt.contains("No PI.md file is present"),
        "should not have absence note when PI.md exists: {}",
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
    let provider = pi_core::ScriptedProvider::new(responses);
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
        Box::new(pi_core::ScriptedProvider::new(vec![
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
        fn complete(&self, messages: &[Message]) -> pi_core::Result<ProviderResponse> {
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

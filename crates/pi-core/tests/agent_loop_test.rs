use pi_core::{
    context::ExecutionContext,
    tools::{make_tools, Tool},
    Agent, Message, ProviderResponse, ScriptedProvider,
};
use tempfile::TempDir;

fn make_test_context() -> (ExecutionContext, TempDir) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    (ExecutionContext::new(root), temp)
}

fn make_tools_for_test(ctx: &ExecutionContext) -> Vec<Box<dyn Tool>> {
    make_tools(ctx).into_values().collect()
}

#[test]
fn test_agent_returns_final_response() {
    let (ctx, _temp) = make_test_context();
    let responses = vec![ProviderResponse::Message(Message::assistant(
        "Hello, how can I help?",
    ))];
    let provider = Box::new(ScriptedProvider::new(responses));
    let mut agent = Agent::new(provider, make_tools_for_test(&ctx));

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
    let provider = Box::new(ScriptedProvider::new(responses));
    let mut agent = Agent::new(provider, make_tools_for_test(&ctx));

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
    let provider = Box::new(ScriptedProvider::new(responses));
    let mut agent = Agent::new(provider, make_tools_for_test(&ctx));

    let result = agent.run(&ctx, "read the file");
    assert!(result.is_ok());
    assert!(result.unwrap().contains("hello world"));
}

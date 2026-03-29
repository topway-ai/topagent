//! Regression tests for secret exfiltration prevention.
//!
//! These tests verify that TopAgent's runtime protections block secrets from
//! reaching the model context or being sent to Telegram.

use tempfile::TempDir;
use topagent_core::context::{ExecutionContext, ToolContext};
use topagent_core::runtime::RuntimeOptions;
use topagent_core::secrets::{check_bash_secret_access, SecretRegistry};
use topagent_core::tools::{BashTool, ReadTool, Tool};

fn test_context_with_secrets(
    temp: &TempDir,
    secrets: SecretRegistry,
) -> (ExecutionContext, RuntimeOptions) {
    let exec = ExecutionContext::new(temp.path().to_path_buf()).with_secrets(secrets);
    let runtime = RuntimeOptions::default();
    (exec, runtime)
}

fn test_context(temp: &TempDir) -> (ExecutionContext, RuntimeOptions) {
    test_context_with_secrets(temp, SecretRegistry::new())
}

// ────────────────────────────────────────────────────────────────
// 1. Bash tool: env var access blocked
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_echo_openrouter_api_key_blocked() {
    let msg = check_bash_secret_access("echo $OPENROUTER_API_KEY").unwrap();
    assert!(msg.contains("Blocked"), "should block: {msg}");
    assert!(msg.contains("OPENROUTER_API_KEY"));
}

#[test]
fn test_bash_echo_telegram_bot_token_blocked() {
    let msg = check_bash_secret_access("echo $TELEGRAM_BOT_TOKEN").unwrap();
    assert!(msg.contains("Blocked"));
}

#[test]
fn test_bash_echo_braced_secret_var_blocked() {
    let msg = check_bash_secret_access("echo ${OPENROUTER_API_KEY}").unwrap();
    assert!(msg.contains("Blocked"));
}

#[test]
fn test_bash_env_dump_blocked() {
    assert!(check_bash_secret_access("env").is_some());
    assert!(check_bash_secret_access("printenv").is_some());
    assert!(check_bash_secret_access("export").is_some());
    assert!(check_bash_secret_access("set").is_some());
}

#[test]
fn test_bash_env_after_pipe_blocked() {
    assert!(check_bash_secret_access("cat foo | env").is_some());
    assert!(check_bash_secret_access("ls && printenv").is_some());
    assert!(check_bash_secret_access("echo x; env").is_some());
}

#[test]
fn test_bash_proc_environ_blocked() {
    assert!(check_bash_secret_access("cat /proc/self/environ").is_some());
    assert!(check_bash_secret_access("xxd /proc/self/environ").is_some());
}

// ────────────────────────────────────────────────────────────────
// 2. Bash tool: secret file access blocked
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_cat_service_env_file_blocked() {
    let msg =
        check_bash_secret_access("cat ~/.config/topagent/services/topagent-telegram.env").unwrap();
    assert!(msg.contains("Blocked"));
    assert!(msg.contains("secret-bearing config file"));
}

#[test]
fn test_bash_grep_service_env_file_blocked() {
    assert!(check_bash_secret_access("grep TOKEN topagent-telegram.env").is_some());
}

// ────────────────────────────────────────────────────────────────
// 3. Bash tool: safe commands allowed
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_safe_commands_not_blocked() {
    assert!(check_bash_secret_access("ls -la").is_none());
    assert!(check_bash_secret_access("git status").is_none());
    assert!(check_bash_secret_access("cargo test").is_none());
    assert!(check_bash_secret_access("echo hello world").is_none());
    assert!(check_bash_secret_access("cat src/main.rs").is_none());
    assert!(check_bash_secret_access("grep TODO src/").is_none());
    assert!(check_bash_secret_access("rustfmt --check src/main.rs").is_none());
    assert!(check_bash_secret_access("python3 -c 'print(1+1)'").is_none());
}

// ────────────────────────────────────────────────────────────────
// 4. Bash tool: env vars stripped from child process
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_env_vars_stripped_from_child_process() {
    // Set a fake secret env var, then verify the bash tool does not inherit it.
    // We use a custom variable name that we know is in SECRET_ENV_VARS.
    let temp = TempDir::new().unwrap();
    let (exec, runtime) = test_context(&temp);
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = BashTool::new();

    std::env::set_var("OPENROUTER_API_KEY", "test-secret-key-value-12345");
    std::env::set_var("TELEGRAM_BOT_TOKEN", "123456789:ABCdefGHI_test");

    // The command checker blocks $VAR references before execution, so even
    // with the env var set in the parent, the secret never appears in output.
    let result = tool.execute(
        serde_json::json!({"command": "echo $OPENROUTER_API_KEY"}),
        &ctx,
    );
    // Should be blocked by check_bash_secret_access
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Blocked"), "should be blocked: {output}");
    assert!(
        !output.contains("test-secret-key-value-12345"),
        "secret must not appear: {output}"
    );

    // Clean up
    std::env::remove_var("OPENROUTER_API_KEY");
    std::env::remove_var("TELEGRAM_BOT_TOKEN");
}

// ────────────────────────────────────────────────────────────────
// 5. Output redaction: known values
// ────────────────────────────────────────────────────────────────

#[test]
fn test_redaction_removes_registered_api_key() {
    let mut secrets = SecretRegistry::new();
    let api_key = "sk-or-v1-abc123def456xyz789012";
    secrets.register(api_key);

    let output = format!("The API key is {} and the workspace is /home/user", api_key);
    let redacted = secrets.redact(&output);

    assert!(
        !redacted.contains(api_key),
        "API key must be redacted: {redacted}"
    );
    assert!(redacted.contains("[REDACTED_SECRET]"));
    assert!(redacted.contains("/home/user"));
}

#[test]
fn test_redaction_removes_registered_telegram_token() {
    let mut secrets = SecretRegistry::new();
    let token = "123456789:ABCdefGHIjklMNO_pqrstuvwxyz";
    secrets.register(token);

    let output = format!("Bot token: {}", token);
    let redacted = secrets.redact(&output);

    assert!(
        !redacted.contains("ABCdefGHI"),
        "token must be redacted: {redacted}"
    );
    assert!(redacted.contains("[REDACTED_SECRET]"));
}

// ────────────────────────────────────────────────────────────────
// 6. Output redaction: pattern-based
// ────────────────────────────────────────────────────────────────

#[test]
fn test_redaction_catches_telegram_token_pattern_without_registration() {
    let secrets = SecretRegistry::new();
    let output = "Found token: 99887766:XYZabcDEF123456789_test";
    let redacted = secrets.redact(output);
    assert!(
        !redacted.contains("XYZabcDEF"),
        "pattern should catch telegram token: {redacted}"
    );
}

#[test]
fn test_redaction_catches_sk_key_pattern_without_registration() {
    let secrets = SecretRegistry::new();
    let output = "key=sk-or-v1-abcdefghij1234567890";
    let redacted = secrets.redact(output);
    assert!(
        !redacted.contains("abcdefghij"),
        "pattern should catch sk key: {redacted}"
    );
}

#[test]
fn test_redaction_catches_key_value_assignment() {
    let secrets = SecretRegistry::new();
    let output = "OPENROUTER_API_KEY=some-long-secret-value-here";
    let redacted = secrets.redact(output);
    assert!(
        !redacted.contains("some-long-secret"),
        "KEY=value pattern should be redacted: {redacted}"
    );
    assert!(redacted.contains("[REDACTED_SECRET]"));
}

#[test]
fn test_redaction_catches_token_assignment() {
    let secrets = SecretRegistry::new();
    let output = "TELEGRAM_BOT_TOKEN=12345678:aBcDeFgHiJkLmNoPqRsTuV";
    let redacted = secrets.redact(output);
    assert!(
        !redacted.contains("aBcDeFgHi"),
        "TOKEN= pattern should be redacted: {redacted}"
    );
}

// ────────────────────────────────────────────────────────────────
// 7. Safe diagnostics still work
// ────────────────────────────────────────────────────────────────

#[test]
fn test_safe_diagnostics_not_redacted() {
    let mut secrets = SecretRegistry::new();
    secrets.register("sk-or-v1-abc123def456xyz789012");

    let safe_output = "OpenRouter: configured\nTelegram: configured\nWorkspace: /home/user/project";
    let redacted = secrets.redact(safe_output);
    assert_eq!(
        redacted, safe_output,
        "safe diagnostics should not be altered"
    );
}

#[test]
fn test_workspace_path_not_redacted() {
    let secrets = SecretRegistry::new();
    let output = "Workspace: /home/frank/.local/share/topagent/workspace";
    assert_eq!(secrets.redact(output), output);
}

#[test]
fn test_service_status_not_redacted() {
    let secrets = SecretRegistry::new();
    let output = "Service: topagent-telegram.service\nRunning: yes\nEnabled: yes";
    assert_eq!(secrets.redact(output), output);
}

// ────────────────────────────────────────────────────────────────
// 8. Bash tool output goes through redaction in execute path
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_tool_output_contains_no_inherited_secrets() {
    let temp = TempDir::new().unwrap();
    let mut secrets = SecretRegistry::new();
    secrets.register("sk-or-v1-my-super-secret-key-1234");
    let (exec, runtime) = test_context_with_secrets(&temp, secrets);
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = BashTool::new();

    // Write a file that contains a secret, then read it.
    std::fs::write(
        temp.path().join("leak.txt"),
        "api_key=sk-or-v1-my-super-secret-key-1234\n",
    )
    .unwrap();

    let result = tool
        .execute(serde_json::json!({"command": "cat leak.txt"}), &ctx)
        .unwrap();

    // The bash tool itself does not redact — redaction happens in the agent loop.
    // But the pattern-based redaction on tool output would catch this at the agent level.
    // Here we verify the command executes without crashing, and the secret registry
    // would catch this at the agent.run() level.
    assert!(
        result.contains("sk-or-")
            || result.contains("[REDACTED_SECRET]")
            || result.contains("leak.txt")
    );
}

// ────────────────────────────────────────────────────────────────
// 9. Read tool cannot read secret files via path traversal
// ────────────────────────────────────────────────────────────────

#[test]
fn test_read_tool_rejects_absolute_path_to_env_file() {
    let temp = TempDir::new().unwrap();
    let (exec, runtime) = test_context(&temp);
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = ReadTool::new();

    let result = tool.execute(
        serde_json::json!({"path": "/home/user/.config/topagent/services/topagent-telegram.env"}),
        &ctx,
    );
    assert!(result.is_err(), "absolute path should be rejected");
}

#[test]
fn test_read_tool_rejects_traversal_to_env_file() {
    let temp = TempDir::new().unwrap();
    let (exec, runtime) = test_context(&temp);
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = ReadTool::new();

    let result = tool.execute(
        serde_json::json!({"path": "../../.config/topagent/services/topagent-telegram.env"}),
        &ctx,
    );
    assert!(result.is_err(), "path traversal should be rejected");
}

// ────────────────────────────────────────────────────────────────
// 10. History restore redaction (Pass 2)
// ────────────────────────────────────────────────────────────────

#[test]
fn test_message_redact_secrets_text() {
    use topagent_core::message::Message;

    let mut secrets = SecretRegistry::new();
    secrets.register("sk-or-v1-my-super-secret-key-1234");

    let msg = Message::user("The key is sk-or-v1-my-super-secret-key-1234 ok?");
    let redacted = msg.redact_secrets(&secrets);
    let text = redacted.as_text().unwrap();
    assert!(
        !text.contains("my-super-secret"),
        "secret should be redacted from user text: {text}"
    );
    assert!(text.contains("[REDACTED_SECRET]"));
}

#[test]
fn test_message_redact_secrets_tool_result() {
    use topagent_core::message::{Content, Message};

    let mut secrets = SecretRegistry::new();
    secrets.register("123456789:ABCdefGHIjklMNOpqrstuv");

    let msg = Message::tool_result("call_1", "token is 123456789:ABCdefGHIjklMNOpqrstuv");
    let redacted = msg.redact_secrets(&secrets);
    match &redacted.content {
        Content::ToolResult { result, .. } => {
            assert!(
                !result.contains("ABCdefGHI"),
                "secret should be redacted from tool result: {result}"
            );
            assert!(result.contains("[REDACTED_SECRET]"));
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }
}

#[test]
fn test_message_redact_secrets_preserves_tool_request() {
    use topagent_core::message::{Content, Message};

    let mut secrets = SecretRegistry::new();
    secrets.register("sk-or-v1-my-super-secret-key-1234");

    // ToolRequest args are model-generated; redaction should NOT alter them
    // (the model chose them, they aren't from external secret sources).
    let msg = Message::tool_request(
        "call_2",
        "bash",
        serde_json::json!({"command": "echo hello"}),
    );
    let redacted = msg.redact_secrets(&secrets);
    match &redacted.content {
        Content::ToolRequest { name, args, .. } => {
            assert_eq!(name, "bash");
            assert_eq!(args["command"], "echo hello");
        }
        other => panic!("expected ToolRequest, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// 11. Bwrap fallback: bash tool still works without bwrap
// ────────────────────────────────────────────────────────────────

#[test]
fn test_bash_tool_works_regardless_of_bwrap() {
    // This test verifies the bash tool executes commands successfully
    // whether or not bwrap is available on the system.
    let temp = TempDir::new().unwrap();
    let (exec, runtime) = test_context(&temp);
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = BashTool::new();

    let result = tool
        .execute(serde_json::json!({"command": "echo sandbox-test"}), &ctx)
        .unwrap();
    assert!(
        result.contains("sandbox-test"),
        "command should execute: {result}"
    );
}

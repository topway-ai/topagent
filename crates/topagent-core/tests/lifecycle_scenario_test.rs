use tempfile::TempDir;
use topagent_core::tools::default_tools;
use topagent_core::{
    context::ExecutionContext, Agent, ApprovalMailbox, ApprovalMailboxMode, ApprovalTriggerKind,
    Error, InfluenceMode, Message, ProviderResponse, Role, RunTrustContext, RuntimeOptions,
    ScriptedProvider, SourceKind, SourceLabel,
};

fn create_temp_crate() -> (TempDir, ExecutionContext) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "lifecycle_fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )
    .unwrap();

    (temp, ExecutionContext::new(root))
}

fn assistant_message(text: &str) -> ProviderResponse {
    ProviderResponse::Message(Message {
        role: Role::Assistant,
        content: topagent_core::Content::Text {
            text: text.to_string(),
        },
    })
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ProviderResponse {
    ProviderResponse::ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        args,
    }
}

fn update_plan_call(id: &str) -> ProviderResponse {
    tool_call(
        id,
        "update_plan",
        serde_json::json!({
            "items": [
                {"content": "Inspect the workspace state", "status": "in_progress"},
                {"content": "Apply the safe fix and verify it", "status": "pending"}
            ]
        }),
    )
}

fn low_trust_context() -> RunTrustContext {
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::low(
        SourceKind::TranscriptPrior,
        InfluenceMode::MayDriveAction,
        "2 prior transcript snippet(s)",
    ));
    trust
}

#[test]
fn test_non_trivial_task_requires_approval_before_destructive_shell() {
    let (_temp, base_ctx) = create_temp_crate();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let ctx = base_ctx.with_approval_mailbox(mailbox.clone());

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "bash",
                "bash",
                serde_json::json!({"command": "touch risky.txt"}),
            ),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let err = agent
        .run(
            &ctx,
            "Make a plan for this repo-wide change, then perform the risky mutation safely.",
        )
        .unwrap_err();

    match err {
        Error::ApprovalRequired(request) => {
            assert_eq!(
                request.action_kind,
                ApprovalTriggerKind::DestructiveShellMutation
            );
        }
        other => panic!("expected approval-required error, got {other:?}"),
    }
    assert!(!ctx.workspace_root.join("risky.txt").exists());
    assert_eq!(mailbox.pending().len(), 1);
}

#[test]
fn test_low_trust_risky_action_gets_elevated_approval_in_real_lifecycle() {
    let (_temp, base_ctx) = create_temp_crate();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let ctx = base_ctx
        .with_run_trust_context(low_trust_context())
        .with_approval_mailbox(mailbox);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "bash",
                "bash",
                serde_json::json!({"command": "touch risky.txt"}),
            ),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let err = agent
        .run(
            &ctx,
            "Use the transcript guidance to make the risky workspace change.",
        )
        .unwrap_err();

    match err {
        Error::ApprovalRequired(request) => {
            assert_eq!(
                request.action_kind,
                ApprovalTriggerKind::DestructiveShellMutation
            );
            assert!(request.reason.contains("prior transcript"));
        }
        other => panic!("expected approval-required error, got {other:?}"),
    }
}

#[test]
fn test_low_trust_context_does_not_block_safe_read_only_operations() {
    let (_temp, base_ctx) = create_temp_crate();
    let ctx = base_ctx.with_run_trust_context(low_trust_context());

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call("read", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("The file contains answer() returning 42."),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let result = agent
        .run(&ctx, "Read the source file and summarize what it does.")
        .unwrap();

    assert!(result.contains("42"));
}

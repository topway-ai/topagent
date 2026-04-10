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
name = "stage_gate_fixture"
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
                {"content": "Edit src/lib.rs", "status": "in_progress"},
                {"content": "Run cargo check --offline", "status": "pending"}
            ]
        }),
    )
}

fn write_lib_call(id: &str, content: &str) -> ProviderResponse {
    tool_call(
        id,
        "write",
        serde_json::json!({
            "path": "src/lib.rs",
            "content": content,
        }),
    )
}

fn cargo_check_call(id: &str) -> ProviderResponse {
    tool_call(
        id,
        "bash",
        serde_json::json!({
            "command": "cargo check --offline",
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
fn test_plan_creation_does_not_bypass_approval_for_risky_bash() {
    let (_temp, ctx) = create_temp_crate();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let ctx = ctx.with_approval_mailbox(mailbox.clone());
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
            "Make a plan for this codebase-wide change, then implement it safely.",
        )
        .unwrap_err();

    match err {
        Error::ApprovalRequired(request) => {
            assert_eq!(
                request.action_kind,
                ApprovalTriggerKind::DestructiveShellMutation
            );
            assert!(!request.reason.contains("planning required"));
        }
        other => panic!("expected approval-required error, got {other:?}"),
    }
    assert!(!ctx.workspace_root.join("risky.txt").exists());
    assert!(!agent.is_planning_gate_active());
    assert_eq!(mailbox.pending().len(), 1);
}

#[test]
fn test_low_trust_influence_survives_to_final_verified_result() {
    let (_temp, ctx) = create_temp_crate();
    let ctx = ctx.with_run_trust_context(low_trust_context());
    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            write_lib_call("write", "pub fn answer() -> u32 {\n    99\n}\n"),
            cargo_check_call("verify"),
            assistant_message("done after verification"),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let result = agent
        .run(&ctx, "apply the transcript-derived fix and verify")
        .unwrap();

    assert!(result.contains("Low-trust content influenced this run"));
    assert!(result.contains("prior transcript"));
    let task_result = agent
        .last_task_result()
        .expect("expected a structured task result");
    assert!(task_result.final_verification_passed());
    assert!(task_result
        .source_labels()
        .iter()
        .any(|source| source.kind == SourceKind::TranscriptPrior));
}

#[test]
fn test_compaction_summary_preserves_trust_notes_and_proof_anchors() {
    let (_temp, ctx) = create_temp_crate();
    let ctx = ctx.with_run_trust_context(low_trust_context());
    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            tool_call(
                "bash-1",
                "bash",
                serde_json::json!({"command": "printf 'pub fn answer() -> u32 {\\n    88\\n}\\n' > src/lib.rs"}),
            ),
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            tool_call("read-2", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("done"),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default().with_max_messages_before_truncation(4),
    );

    let result = agent
        .run(
            &ctx,
            "update src/lib.rs via a transcript-derived shell command",
        )
        .unwrap();

    assert!(result.contains("Low-trust content influenced this run"));
    assert!(result.contains("Files were modified but no verification commands were run"));

    let summary = agent
        .conversation_messages()
        .into_iter()
        .find_map(|message| {
            message
                .as_text()
                .filter(|text| text.starts_with("["))
                .map(str::to_string)
        })
        .expect("compaction summary should be present");
    assert!(summary.contains("Trust notes:"));
    assert!(summary.contains("Low-trust content is active in this run"));
    assert!(summary.contains("Files were modified but no verification commands were run"));
}

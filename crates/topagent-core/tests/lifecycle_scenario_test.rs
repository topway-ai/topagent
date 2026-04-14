use std::collections::BTreeMap;
use tempfile::TempDir;
use topagent_core::tools::default_tools;
use topagent_core::{
    context::ExecutionContext,
    hooks::{HookDefinition, HookEvent, HookManifest, HookRegistry},
    tool_genesis::{ToolGenesis, VerificationSpec},
    Agent, ApprovalMailbox, ApprovalMailboxMode, ApprovalTriggerKind, Error, InfluenceMode,
    Message, ProviderResponse, Role, RunTrustContext, RuntimeOptions, ScriptedProvider, SourceKind,
    SourceLabel,
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
                {"content": "Inspect the generated tool and workspace state", "status": "in_progress"},
                {"content": "Apply the safe fix and verify it", "status": "pending"}
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

fn create_drifted_generated_tool(workspace: &TempDir) {
    let genesis = ToolGenesis::new(workspace.path().to_path_buf());
    genesis
        .create_tool(
            "drifted_tool",
            "verified helper",
            "echo original",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("original".to_string()),
            }),
        )
        .unwrap();
    std::fs::write(
        workspace
            .path()
            .join(".topagent/tools/drifted_tool/script.sh"),
        "echo tampered",
    )
    .unwrap();
}

#[test]
fn test_non_trivial_one_shot_task_plans_then_requires_approval_without_touching_unused_generated_tool(
) {
    let (temp, base_ctx) = create_temp_crate();
    create_drifted_generated_tool(&temp);
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
    assert!(
        agent.external_tools().get("drifted_tool").is_some(),
        "unused generated tool should stay on the cheap runtime path"
    );
}

#[test]
fn test_generated_tool_revalidation_on_use_allows_manual_verified_recovery() {
    let (temp, ctx) = create_temp_crate();
    create_drifted_generated_tool(&temp);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call("use", "drifted_tool", serde_json::json!({})),
            write_lib_call("write", "pub fn answer() -> u32 {\n    77\n}\n"),
            cargo_check_call("verify"),
            assistant_message("manual recovery complete"),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let result = agent
        .run(
            &ctx,
            "Plan the fix, try the helper, recover manually if needed, and verify the result.",
        )
        .unwrap();

    assert!(result.contains("manual recovery complete"));
    assert!(result.contains("Workspace Warnings"));
    assert!(result.contains("drifted_tool: script.sh changed after approval"));
    assert!(
        agent.external_tools().get("drifted_tool").is_none(),
        "tampered generated tool should be removed after failed revalidation"
    );

    let task_result = agent
        .last_task_result()
        .expect("expected a structured task result");
    assert!(task_result.final_verification_passed());
    assert!(task_result
        .files_changed()
        .contains(&"src/lib.rs".to_string()));
    assert!(task_result
        .evidence
        .workspace_warnings
        .iter()
        .any(|warning| warning.contains("drifted_tool")));
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
fn test_approval_gate_fires_before_hooks_for_destructive_mutations() {
    let (temp, base_ctx) = create_temp_crate();

    // Write a permissive hook that would allow everything
    let script_path = temp.path().join("allow-all.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho '{\"action\": \"allow\"}'\n").unwrap();

    let manifest = HookManifest {
        hooks: vec![HookDefinition {
            event: HookEvent::PreTool,
            command: format!("sh {}", script_path.display()),
            filter: vec!["bash".to_string()],
            label: "permissive hook".to_string(),
            timeout_secs: 5,
        }],
    };
    let registry = HookRegistry::from_manifest(manifest);

    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let ctx = base_ctx
        .with_approval_mailbox(mailbox.clone())
        .with_hook_registry(registry);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "rm-bash",
                "bash",
                serde_json::json!({"command": "rm -rf /tmp/test"}),
            ),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    // Approval gate fires before hooks — the permissive hook cannot bypass approval
    let err = agent
        .run(
            &ctx,
            "Plan the cleanup, then remove the temporary test directory.",
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
    assert!(!ctx.workspace_root.join("tmp/test").exists());
    assert_eq!(mailbox.pending().len(), 1);
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

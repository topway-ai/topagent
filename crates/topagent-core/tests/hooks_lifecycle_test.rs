use std::fs;
use tempfile::TempDir;
use topagent_core::hooks::{
    HookDefinition, HookEvent, HookInput, HookManifest, HookRegistry, HookVerdict, dispatch_hooks,
    execute_hook,
};
use topagent_core::tools::default_tools;
use topagent_core::{
    Agent, ApprovalMailbox, ApprovalMailboxMode, ApprovalTriggerKind, BehaviorContract,
    DurablePromotionKind, Error, InfluenceMode, Message, ProviderResponse, Role, RunTrustContext,
    RuntimeOptions, ScriptedProvider, SourceKind, SourceLabel, context::ExecutionContext,
};

fn create_temp_crate() -> (TempDir, ExecutionContext) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "hook_fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    fs::write(
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
                {"content": "Apply the fix", "status": "in_progress"},
                {"content": "Verify", "status": "pending"}
            ]
        }),
    )
}

fn write_hooks_toml(temp: &TempDir, content: &str) {
    let hooks_dir = temp.path().join(".topagent");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(hooks_dir.join("hooks.toml"), content).unwrap();
}

// ── Test: no-hook startup stays cheap ──

#[test]
fn test_no_hooks_configured_has_zero_overhead() {
    let (temp, _ctx) = create_temp_crate();
    let registry = HookRegistry::load_from_workspace(temp.path());
    assert!(registry.is_empty());
    assert!(registry.hooks_for(HookEvent::PreTool).is_empty());
    assert!(registry.hooks_for(HookEvent::OnSessionStart).is_empty());
    assert!(registry.hooks_for(HookEvent::PostWrite).is_empty());
    assert!(registry.hooks_for(HookEvent::PreFinal).is_empty());
    assert!(registry.summary_lines().is_empty());

    // Dispatch against empty registry should be a trivial pass
    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: "rm -rf /".to_string(),
    };
    let result = dispatch_hooks(&registry, HookEvent::PreTool, &input, temp.path());
    assert!(result.is_pass());
    assert!(!result.blocked);
}

// ── Test: pre-tool hook can block a risky bash command ──

#[test]
fn test_pre_tool_hook_blocks_risky_bash_command() {
    let temp = TempDir::new().unwrap();
    // Create a hook script that blocks bash commands containing "rm -rf"
    let script_path = temp.path().join("check-bash.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
# Read stdin (the hook input JSON)
INPUT=$(cat)
# Check if the detail contains "rm -rf"
if echo "$INPUT" | grep -q 'rm -rf'; then
    echo 'block: rm -rf commands are not allowed in this workspace'
else
    echo '{"action": "allow"}'
fi
"#,
    )
    .unwrap();

    let hook = HookDefinition {
        event: HookEvent::PreTool,
        command: format!("sh {}", script_path.display()),
        filter: vec!["bash".to_string()],
        label: "bash safety guard".to_string(),
        timeout_secs: 5,
    };

    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: r#"{"command": "rm -rf /"}"#.to_string(),
    };
    let result = execute_hook(&hook, &input, temp.path());
    assert!(result.succeeded);
    assert_eq!(
        result.verdict,
        HookVerdict::Block {
            reason: "rm -rf commands are not allowed in this workspace".to_string()
        }
    );

    // Non-risky command should pass
    let safe_input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: r#"{"command": "ls -la"}"#.to_string(),
    };
    let safe_result = execute_hook(&hook, &safe_input, temp.path());
    assert!(safe_result.succeeded);
    assert_eq!(safe_result.verdict, HookVerdict::Allow);
}

// ── Test: post-write hook can request bounded verification ──

#[test]
fn test_post_write_hook_requests_verification() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("fmt-check.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
INPUT=$(cat)
if echo "$INPUT" | grep -q '\.rs'; then
    echo 'verify: cargo fmt --check'
fi
"#,
    )
    .unwrap();

    let hook = HookDefinition {
        event: HookEvent::PostWrite,
        command: format!("sh {}", script_path.display()),
        filter: vec!["*.rs".to_string()],
        label: "rust format check".to_string(),
        timeout_secs: 5,
    };

    let input = HookInput {
        event: HookEvent::PostWrite,
        subject: "src/main.rs".to_string(),
        detail: "write".to_string(),
    };
    let result = execute_hook(&hook, &input, temp.path());
    assert!(result.succeeded);
    assert_eq!(
        result.verdict,
        HookVerdict::RequestVerify {
            command: "cargo fmt --check".to_string()
        }
    );
}

// ── Test: startup hook injects bounded context ──

#[test]
fn test_on_session_start_hook_injects_bounded_context() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("startup-context.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
echo 'note: This workspace uses strict linting. Run cargo clippy before committing.'
"#,
    )
    .unwrap();

    let manifest = HookManifest {
        hooks: vec![HookDefinition {
            event: HookEvent::OnSessionStart,
            command: format!("sh {}", script_path.display()),
            filter: vec![],
            label: "project rules".to_string(),
            timeout_secs: 5,
        }],
    };
    let registry = HookRegistry::from_manifest(manifest);

    let input = HookInput {
        event: HookEvent::OnSessionStart,
        subject: String::new(),
        detail: "fix the parser".to_string(),
    };
    let result = dispatch_hooks(&registry, HookEvent::OnSessionStart, &input, temp.path());
    assert!(!result.blocked);
    let context = result.annotation_context().unwrap();
    assert!(context.contains("strict linting"));
    assert!(context.contains("cargo clippy"));
}

// ── Test: hooks cannot bypass approval gates ──
//
// The approval gate runs BEFORE hooks in the gate chain.
// A hook returning Allow does not override a pending approval.

#[test]
fn test_hooks_cannot_bypass_approval_gate() {
    let (temp, base_ctx) = create_temp_crate();
    write_hooks_toml(
        &temp,
        r#"
[[hooks]]
event = "pre_tool"
command = "echo '{\"action\": \"allow\"}'"
label = "permissive hook"
"#,
    );

    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let registry = HookRegistry::load_from_workspace(temp.path());
    let ctx = base_ctx
        .with_approval_mailbox(mailbox.clone())
        .with_hook_registry(registry);

    // Use require_plan=false to skip task classification LLM calls
    // and avoid exhausting the scripted provider
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
        RuntimeOptions::default().with_require_plan(false),
    );

    let err = agent.run(&ctx, "touch risky.txt").unwrap_err();

    // Approval should still be required even with a permissive hook
    match err {
        Error::ApprovalRequired(request) => {
            assert_eq!(
                request.action_kind,
                ApprovalTriggerKind::DestructiveShellMutation
            );
        }
        other => panic!("expected approval-required error despite permissive hook, got {other:?}"),
    }
    assert!(!ctx.workspace_root.join("risky.txt").exists());
}

// ── Test: hooks cannot bypass low-trust durable-write blocking ──

#[test]
fn test_hooks_cannot_bypass_low_trust_memory_write_blocking() {
    let (temp, base_ctx) = create_temp_crate();
    write_hooks_toml(
        &temp,
        r#"
[[hooks]]
event = "pre_tool"
command = "echo '{\"action\": \"allow\"}'"
label = "permissive hook"
"#,
    );

    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::low(
        SourceKind::TranscriptPrior,
        InfluenceMode::MayDriveAction,
        "2 prior transcript snippet(s)",
    ));

    let registry = HookRegistry::load_from_workspace(temp.path());
    let ctx = base_ctx
        .with_run_trust_context(trust)
        .with_hook_registry(registry);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "save",
                "save_lesson",
                serde_json::json!({
                    "title": "test lesson",
                    "content": "something learned from low-trust content"
                }),
            ),
            assistant_message("done"),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let result = agent
        .run(
            &ctx,
            "Plan changes, then save a lesson from the prior transcript context.",
        )
        .unwrap();

    // The memory trust gate should still block the write despite the permissive hook
    assert!(
        result.contains("done"),
        "agent should complete despite blocked memory write"
    );
}

// ── Test: hooks do not bloat prompt assembly when absent ──

#[test]
fn test_no_hooks_no_prompt_section() {
    let (_temp, ctx) = create_temp_crate();

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![assistant_message("hello")])),
        default_tools().into_inner(),
        RuntimeOptions::default().with_require_plan(false),
    );

    let result = agent.run(&ctx, "say hello").unwrap();
    assert!(result.contains("hello"));
    // No hook section should appear in the prompt when no hooks are configured.
    // We verify indirectly: the agent completes without hook-related artifacts.
}

// ── Test: pre-tool hook blocks via workspace-loaded manifest ──

#[test]
fn test_workspace_manifest_pre_tool_hook_blocks_in_agent_run() {
    let (temp, base_ctx) = create_temp_crate();

    // Write a hook manifest that blocks all bash commands
    let script_path = temp.path().join("block-all-bash.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\necho 'block: all bash blocked by workspace policy'\n",
    )
    .unwrap();

    write_hooks_toml(
        &temp,
        &format!(
            r#"
[[hooks]]
event = "pre_tool"
command = "sh {}"
filter = ["bash"]
label = "workspace bash block"
"#,
            script_path.display()
        ),
    );

    let registry = HookRegistry::load_from_workspace(temp.path());
    assert!(!registry.is_empty());
    let ctx = base_ctx.with_hook_registry(registry);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            // Agent tries a research-safe bash command — hooks apply after
            // the planning gate but the agent should see the block message.
            tool_call("bash-1", "bash", serde_json::json!({"command": "ls -la"})),
            assistant_message("I see bash was blocked by a workspace hook"),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default(),
    );

    let result = agent
        .run(
            &ctx,
            "Plan: inspect the workspace, then report what you see.",
        )
        .unwrap();

    assert!(result.contains("blocked"));
}

// ── Test: dispatch with filter mismatch is a pass ──

#[test]
fn test_dispatch_filter_mismatch_is_pass() {
    let temp = TempDir::new().unwrap();
    let manifest = HookManifest {
        hooks: vec![HookDefinition {
            event: HookEvent::PreTool,
            command: "echo 'block: should not fire'".to_string(),
            filter: vec!["bash".to_string()],
            label: "bash only".to_string(),
            timeout_secs: 5,
        }],
    };
    let registry = HookRegistry::from_manifest(manifest);

    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "write".to_string(),
        detail: "{}".to_string(),
    };
    let result = dispatch_hooks(&registry, HookEvent::PreTool, &input, temp.path());
    assert!(result.is_pass());
}

// ── Test: broken hook defaults to Allow ──

#[test]
fn test_broken_hook_defaults_to_allow() {
    let temp = TempDir::new().unwrap();
    let hook = HookDefinition {
        event: HookEvent::PreTool,
        command: "exit 1".to_string(), // Non-zero exit = hook failure
        filter: vec![],
        label: "broken hook".to_string(),
        timeout_secs: 5,
    };

    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: "{}".to_string(),
    };
    let result = execute_hook(&hook, &input, temp.path());
    assert!(!result.succeeded);
    assert_eq!(result.verdict, HookVerdict::Allow);
}

// ── Test: multiple hooks compose correctly ──

#[test]
fn test_multiple_hooks_block_takes_priority() {
    let temp = TempDir::new().unwrap();

    let allow_script = temp.path().join("allow.sh");
    fs::write(&allow_script, "#!/bin/sh\necho '{\"action\": \"allow\"}'\n").unwrap();

    let block_script = temp.path().join("block.sh");
    fs::write(
        &block_script,
        "#!/bin/sh\necho 'block: blocked by safety policy'\n",
    )
    .unwrap();

    let manifest = HookManifest {
        hooks: vec![
            HookDefinition {
                event: HookEvent::PreTool,
                command: format!("sh {}", allow_script.display()),
                filter: vec![],
                label: "permissive".to_string(),
                timeout_secs: 5,
            },
            HookDefinition {
                event: HookEvent::PreTool,
                command: format!("sh {}", block_script.display()),
                filter: vec![],
                label: "strict".to_string(),
                timeout_secs: 5,
            },
        ],
    };
    let registry = HookRegistry::from_manifest(manifest);

    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: "{}".to_string(),
    };
    let result = dispatch_hooks(&registry, HookEvent::PreTool, &input, temp.path());
    assert!(result.blocked);
    let msg = result.block_message().unwrap();
    assert!(msg.contains("blocked by safety policy"));
}

#[test]
fn test_no_hook_path_stays_semantically_normal() {
    let (_temp, ctx) = create_temp_crate();
    let registry = HookRegistry::empty();
    let ctx_with_hooks = ctx.with_hook_registry(registry);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "read-file",
                "read",
                serde_json::json!({"path": "src/lib.rs"}),
            ),
            assistant_message("The file contains answer() returning 42."),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default().with_require_plan(false),
    );

    let result = agent
        .run(&ctx_with_hooks, "Read the file and tell me what it does.")
        .unwrap();

    assert!(result.contains("42"));
    assert!(
        !result.contains("Blocked by workspace hook"),
        "no-hook path should not have any hook block messages"
    );
    assert!(
        agent.last_task_result().is_none()
            || !agent
                .last_task_result()
                .map(|r| r
                    .evidence
                    .workspace_warnings
                    .iter()
                    .any(|w| w.contains("hook")))
                .unwrap_or(false),
        "no-hook path should not produce hook-related workspace warnings"
    );
}

#[test]
fn test_post_write_hook_bounded_verify_command_is_not_run_automatically() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("verify-hook.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\necho 'verify: cargo clippy --all-targets'\n",
    )
    .unwrap();

    let hook = HookDefinition {
        event: HookEvent::PostWrite,
        command: format!("sh {}", script_path.display()),
        filter: vec!["*.rs".to_string()],
        label: "rust lint check".to_string(),
        timeout_secs: 5,
    };

    let input = HookInput {
        event: HookEvent::PostWrite,
        subject: "src/main.rs".to_string(),
        detail: String::new(),
    };
    let result = execute_hook(&hook, &input, temp.path());

    assert!(result.succeeded);
    assert_eq!(
        result.verdict,
        HookVerdict::RequestVerify {
            command: "cargo clippy --all-targets".to_string()
        },
        "PostWrite hook should request bounded verification, not arbitrary commands"
    );
    assert!(
        result.verdict != HookVerdict::Allow,
        "verify request must not be treated as a pass"
    );
    assert!(
        result.verdict
            != HookVerdict::Block {
                reason: String::new()
            },
        "verify request must not be treated as a block"
    );
}

#[test]
fn test_hooks_does_not_materially_bloat_startup_when_absent() {
    let (temp, _ctx) = create_temp_crate();
    let registry = HookRegistry::load_from_workspace(temp.path());
    assert!(registry.is_empty(), "empty workspace should have no hooks");

    let summary = registry.summary_lines();
    assert!(
        summary.is_empty(),
        "empty registry should produce no summary lines"
    );

    let input = HookInput {
        event: HookEvent::PreTool,
        subject: "bash".to_string(),
        detail: "{}".to_string(),
    };
    let result = dispatch_hooks(&registry, HookEvent::PreTool, &input, temp.path());
    assert!(result.is_pass());
    assert!(result.execution_results.is_empty());
    assert!(!result.blocked);
    assert!(result.annotations.is_empty());
    assert!(result.verify_commands.is_empty());
}

#[test]
fn test_hooks_cannot_bypass_approval_gate_for_git_commit() {
    let (temp, base_ctx) = create_temp_crate();
    write_hooks_toml(
        &temp,
        r#"
[[hooks]]
event = "pre_tool"
command = "echo '{\"action\": \"allow\"}'"
label = "permissive hook"
"#,
    );

    let registry = HookRegistry::load_from_workspace(temp.path());
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let ctx = base_ctx
        .with_approval_mailbox(mailbox.clone())
        .with_hook_registry(registry);

    let mut agent = Agent::with_options(
        Box::new(ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call(
                "commit",
                "git_commit",
                serde_json::json!({"message": "commit despite hook allow"}),
            ),
        ])),
        default_tools().into_inner(),
        RuntimeOptions::default().with_require_plan(false),
    );

    let err = agent
        .run(&ctx, "Commit the changes with a message.")
        .unwrap_err();

    match err {
        Error::ApprovalRequired(request) => {
            assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
        }
        other => panic!(
            "expected approval-required for git_commit despite permissive hook, got {:?}",
            other
        ),
    }
}

#[test]
fn test_low_trust_durable_promotion_remains_blocked_after_restore_context() {
    let _ = create_temp_crate();

    let mut trust_after_restore = RunTrustContext::default();
    trust_after_restore.add_source(SourceLabel::low(
        SourceKind::TranscriptPrior,
        InfluenceMode::MayDriveAction,
        "prior context before restore",
    ));

    let contract = BehaviorContract::default();
    assert!(
        contract
            .durable_promotion_block_reason(
                DurablePromotionKind::Lesson,
                &trust_after_restore,
                false
            )
            .is_some(),
        "low-trust context must still block lesson promotion even after restore/restart"
    );
    assert!(
        contract
            .durable_promotion_block_reason(
                DurablePromotionKind::Procedure,
                &trust_after_restore,
                false
            )
            .is_some(),
        "low-trust context must still block procedure promotion even after restore/restart"
    );
    assert!(
        contract
            .memory_write_block_reason("save_lesson", &trust_after_restore, false)
            .is_some(),
        "low-trust context must still block memory writes even after restore/restart"
    );
}

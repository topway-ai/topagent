use super::*;
use crate::context::{ExecutionContext, ToolContext};
use crate::runtime::RuntimeOptions;
use crate::tools::{Tool, ToolRegistry};
use tempfile::TempDir;

#[test]
fn test_register_generated_tool_authoring_tools_adds_expected_specs() {
    let mut registry = ToolRegistry::new();
    register_generated_tool_authoring_tools(&mut registry);
    let names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"create_tool".to_string()));
    assert!(names.contains(&"repair_tool".to_string()));
    assert!(names.contains(&"list_generated_tools".to_string()));
    assert!(names.contains(&"delete_generated_tool".to_string()));
}

#[test]
fn test_tool_genesis_create_and_verify() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "echo_hello",
            "echo hello",
            "echo hello",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("hello".to_string()),
            }),
        )
        .unwrap();

    assert!(result.success);
    assert_eq!(result.tool_name, "echo_hello");
    assert!(result.verification_passed);

    let tools = genesis.load_verified_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "echo_hello");
    assert_eq!(tools[0].sandbox_policy(), CommandSandboxPolicy::Workspace);
    assert!(tools[0].spec().description.contains("sandboxed workspace"));
}

#[test]
fn test_tool_genesis_fails_verification() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "failing_tool",
            "fails",
            "exit 1",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: None,
            }),
        )
        .unwrap();

    assert!(!result.success);
    assert!(!result.verification_passed);
}

#[test]
fn test_generated_tool_verification_strips_secret_env() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    std::env::set_var("OPENROUTER_API_KEY", "sensitive-openrouter-secret");
    let result = genesis
        .create_tool(
            "env_probe",
            "probe env",
            "printf %s \"$OPENROUTER_API_KEY\"",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("sensitive-openrouter-secret".to_string()),
            }),
        )
        .unwrap();
    std::env::remove_var("OPENROUTER_API_KEY");

    assert!(
        !result.verification_passed,
        "generated tool verification must not inherit secret env vars"
    );
}

#[test]
fn test_list_generated_tools() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "tool_one",
            "first",
            "echo one",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("one".to_string()),
            }),
        )
        .unwrap();

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "tool_one");
    assert!(tools[0].load_warning.is_none());
}

#[test]
fn test_create_tool_persists_only_manifest_and_script() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "persisted_tool",
            "test persistence",
            "echo ok",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        )
        .unwrap();

    let tool_dir = temp.path().join(".topagent/tools/persisted_tool");
    let manifest_json = std::fs::read_to_string(tool_dir.join("manifest.json")).unwrap();
    assert!(tool_dir.join("script.sh").exists());
    assert!(
        !manifest_json.contains("\"command\""),
        "generated tool manifests should not duplicate the shell script body"
    );
    assert!(
        !temp.path().join(".topagent/tool-genesis").exists(),
        "generated tool creation should not leave behind a second persistence tree"
    );
}

#[test]
fn test_create_tool_name_validation() {
    let temp = TempDir::new().unwrap();
    let exec = ExecutionContext::new(temp.path().to_path_buf());
    let runtime = RuntimeOptions::default();
    let ctx = ToolContext::new(&exec, &runtime);
    let tool = CreateToolTool::new();

    let bad_args = serde_json::json!({
        "name": "bad/name",
        "description": "test",
        "script": "echo hi"
    });

    let result = tool.execute(bad_args, &ctx);
    assert!(result.is_err());
}

#[test]
fn test_repair_tool_name_validation() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis.repair_tool(
        "../bad_name",
        "echo fixed",
        None,
        None,
        Some(&VerificationSpec {
            verification_inputs: BTreeMap::new(),
            expected_exit: 0,
            expected_output_contains: None,
        }),
    );

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("tool name must be alphanumeric + underscore only"));
}

#[test]
fn test_delete_tool_name_validation() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis.delete_generated_tool("../bad_name");

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("tool name must be alphanumeric + underscore only"));
}

#[test]
fn test_create_tool_rejects_invalid_interface_before_writing_files() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis.create_tool(
        "invalid_interface",
        "broken interface",
        "echo ok",
        vec![ToolInput {
            name: "msg".to_string(),
            description: "message".to_string(),
        }],
        vec!["{missing}".to_string()],
        Some(VerificationSpec {
            verification_inputs: BTreeMap::from([("msg".to_string(), "ok".to_string())]),
            expected_exit: 0,
            expected_output_contains: Some("ok".to_string()),
        }),
    );

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("has no matching input"));
    assert!(
        !temp
            .path()
            .join(".topagent/tools/invalid_interface")
            .exists(),
        "invalid tools should fail before any on-disk artifact is created"
    );
}

#[test]
fn test_structured_generated_tool_verifies_with_named_inputs() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "named_verify",
            "echo args tool",
            "printf 'hello %s' \"$1\"",
            vec![ToolInput {
                name: "name".to_string(),
                description: "name to greet".to_string(),
            }],
            vec!["{name}".to_string()],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::from([("name".to_string(), "world".to_string())]),
                expected_exit: 0,
                expected_output_contains: Some("hello world".to_string()),
            }),
        )
        .unwrap();

    assert!(result.success);
    assert!(result.verification_passed);
}

#[test]
fn test_create_and_repair_tool() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "repairable",
            "initially broken",
            "exit 1",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: None,
            }),
        )
        .unwrap();

    assert!(!result.success);

    let repair_result = genesis
        .repair_tool(
            "repairable",
            "echo fixed",
            None,
            None,
            Some(&VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("fixed".to_string()),
            }),
        )
        .unwrap();

    assert!(repair_result.success);
    assert_eq!(repair_result.repair_attempts, 1);
}

#[test]
fn test_verification_invokes_generated_script() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let script = "echo SCRIPT_OUTPUT";
    let result = genesis
        .create_tool(
            "script_check",
            "checks script runs",
            script,
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("SCRIPT_OUTPUT".to_string()),
            }),
        )
        .unwrap();

    assert!(
        result.success,
        "verification should pass when script output matches"
    );
    assert!(result.verification_passed);
}

#[test]
fn test_incomplete_artifact_not_promoted() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "incomplete_tool",
            "incomplete",
            "echo test",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("test".to_string()),
            }),
        )
        .unwrap();

    let tool_dir = temp.path().join(".topagent/tools/incomplete_tool");
    let manifest_path = tool_dir.join("manifest.json");
    let script_path = tool_dir.join("script.sh");

    let mut manifest: ToolManifest =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest.verified = true;
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(&script_path).unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert!(
        loaded.is_empty(),
        "tool with missing script should not be loaded even if manifest says verified"
    );

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "incomplete_tool");
    assert_eq!(tools[0].load_warning.as_deref(), Some("missing script.sh"));
}

#[test]
fn test_verified_tool_without_script_hash_is_unavailable() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "verified_missing_hash",
            "verified tool missing hash",
            "echo ok",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        )
        .unwrap();

    let manifest_path = temp
        .path()
        .join(".topagent/tools/verified_missing_hash/manifest.json");
    let mut manifest: ToolManifest =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest.script_sha256 = None;
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert!(loaded.is_empty());

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "verified_missing_hash");
    assert_eq!(
        tools[0].load_warning.as_deref(),
        Some("missing script_sha256; repair or recreate the tool to make it usable")
    );
}

#[test]
fn test_verified_tool_becomes_unavailable_if_script_changes_after_verification() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "tampered_tool",
            "tampered tool",
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
        temp.path().join(".topagent/tools/tampered_tool/script.sh"),
        "echo tampered",
    )
    .unwrap();

    let runtime_loaded = genesis.load_verified_tools().unwrap();
    assert_eq!(
        runtime_loaded.len(),
        1,
        "cheap runtime inventory should still load the tool before explicit use"
    );

    let maintained_inventory = genesis.generated_tool_inventory().unwrap();
    assert!(
        maintained_inventory.verified_tools.is_empty(),
        "explicit maintenance scan should withhold the tampered tool from callable inventory"
    );

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "tampered_tool");
    assert_eq!(
        tools[0].load_warning.as_deref(),
        Some("script.sh changed after verification; repair or recreate the tool")
    );
}

#[test]
fn test_verification_fails_for_wrong_output() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "wrong_output",
            "fails output check",
            "echo ACTUAL_OUTPUT",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("WRONG_OUTPUT".to_string()),
            }),
        )
        .unwrap();

    assert!(
        !result.success,
        "verification should fail when output does not match"
    );
    assert!(!result.verification_passed);
}

#[test]
fn test_repair_is_single_step() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "single_repair",
            "broken",
            "exit 1",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: None,
            }),
        )
        .unwrap();

    let repair_result = genesis
        .repair_tool(
            "single_repair",
            "exit 1",
            None,
            None,
            Some(&VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: None,
            }),
        )
        .unwrap();

    assert!(
        !repair_result.success,
        "second repair step should still fail for same broken script"
    );
    assert_eq!(
        repair_result.repair_attempts, 1,
        "repair_attempts should be exactly 1 (manual single-step)"
    );
}

#[test]
fn test_corrupt_manifest_not_loaded_as_verified() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let tool_dir = temp.path().join(".topagent/tools/bad_manifest");
    std::fs::create_dir_all(&tool_dir).unwrap();
    std::fs::write(tool_dir.join("manifest.json"), "not valid json{{{").unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert!(loaded.is_empty(), "corrupt manifest should not be loaded");

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "bad_manifest");
    assert!(tools[0]
        .load_warning
        .as_deref()
        .unwrap_or_default()
        .contains("invalid manifest.json"));
}

#[test]
fn test_structured_generated_tool_reusable_with_spaces() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "echo_args",
            "echo args tool",
            "echo \"$@\"",
            vec![ToolInput {
                name: "msg".to_string(),
                description: "message to echo".to_string(),
            }],
            vec!["{msg}".to_string()],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::from([("msg".to_string(), "test".to_string())]),
                expected_exit: 0,
                expected_output_contains: Some("test".to_string()),
            }),
        )
        .unwrap();

    assert!(
        result.success,
        "tool creation and verification should succeed"
    );

    let loaded = genesis.load_verified_tools().unwrap();
    assert_eq!(loaded.len(), 1);

    let tool = &loaded[0];
    assert_eq!(tool.spec().name, "echo_args");

    let exec = ExecutionContext::new(temp.path().to_path_buf());
    let runtime = RuntimeOptions::default();
    let ctx = ToolContext::new(&exec, &runtime);
    let result = tool.execute(&serde_json::json!({"msg": "hello world with spaces"}), &ctx);
    assert!(result.is_ok());
    let output = result.unwrap().output;
    assert!(output.contains("hello world with spaces"));
}

#[test]
fn test_structured_generated_tool_reusable_special_chars() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "printf_special",
            "printf tool",
            "printf '%s' \"$1\"",
            vec![ToolInput {
                name: "arg".to_string(),
                description: "argument".to_string(),
            }],
            vec!["{arg}".to_string()],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::from([("arg".to_string(), "ok".to_string())]),
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        )
        .unwrap();

    assert!(result.success);

    let loaded = genesis.load_verified_tools().unwrap();
    assert_eq!(loaded.len(), 1);

    let tool = &loaded[0];
    let exec = ExecutionContext::new(temp.path().to_path_buf());
    let runtime = RuntimeOptions::default();
    let ctx = ToolContext::new(&exec, &runtime);
    let result = tool.execute(&serde_json::json!({"arg": "$HOME/foo --bar"}), &ctx);
    assert!(result.is_ok());
    let output = result.unwrap().output;
    assert!(output.contains("$HOME/foo --bar"));
}

#[test]
fn test_delete_generated_tool_removes_from_set() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "to_delete",
            "will be deleted",
            "echo ok",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        )
        .unwrap();

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);

    genesis.delete_generated_tool("to_delete").unwrap();

    let tools = genesis.list_generated_tools().unwrap();
    assert!(tools.is_empty());
}

#[test]
fn test_delete_missing_tool_fails() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis.delete_generated_tool("nonexistent");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn test_delete_removes_verified_tool() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "verified_tool",
            "verified tool",
            "echo verified",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("verified".to_string()),
            }),
        )
        .unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert_eq!(loaded.len(), 1);

    genesis.delete_generated_tool("verified_tool").unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_delete_and_recreate_replaces_tool() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "replaceable",
            "original",
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

    genesis.delete_generated_tool("replaceable").unwrap();

    genesis
        .create_tool(
            "replaceable",
            "replacement",
            "echo replacement",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("replacement".to_string()),
            }),
        )
        .unwrap();

    let tools = genesis.list_generated_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].description, "replacement");
}

#[test]
fn test_bad_tool_can_be_deleted() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    let result = genesis
        .create_tool(
            "bad_tool",
            "broken tool",
            "exit 1",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: None,
            }),
        )
        .unwrap();

    assert!(!result.success);

    genesis.delete_generated_tool("bad_tool").unwrap();

    let tools = genesis.list_generated_tools().unwrap();
    assert!(tools.is_empty());
}

#[test]
fn test_structured_tool_can_be_deleted() {
    let temp = TempDir::new().unwrap();
    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "echo_tool",
            "echo args tool",
            "echo \"$@\"",
            vec![ToolInput {
                name: "msg".to_string(),
                description: "message to echo".to_string(),
            }],
            vec!["{msg}".to_string()],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::from([("msg".to_string(), "test".to_string())]),
                expected_exit: 0,
                expected_output_contains: Some("test".to_string()),
            }),
        )
        .unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert_eq!(loaded.len(), 1);

    genesis.delete_generated_tool("echo_tool").unwrap();

    let loaded = genesis.load_verified_tools().unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_delete_generated_tool_via_tool() {
    let temp = TempDir::new().unwrap();
    let exec = ExecutionContext::new(temp.path().to_path_buf());
    let runtime = RuntimeOptions::default();
    let ctx = ToolContext::new(&exec, &runtime);

    let genesis = ToolGenesis::new(temp.path().to_path_buf());

    genesis
        .create_tool(
            "tool_to_delete",
            "will be deleted via tool",
            "echo ok",
            vec![],
            vec![],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::new(),
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        )
        .unwrap();

    let tool = DeleteGeneratedToolTool::new();
    let result = tool.execute(serde_json::json!({"name": "tool_to_delete"}), &ctx);
    assert!(result.is_ok());
    assert!(result.unwrap().contains("deleted"));

    let tools = genesis.list_generated_tools().unwrap();
    assert!(tools.is_empty());
}

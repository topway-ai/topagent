use std::fs;
use std::path::{Path, PathBuf};

const RAW_EXECUTE_ALLOWLIST: &str = "topagent-allow-agent-raw-execute:";

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn collect_rust_files(root: &Path, paths: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(root).unwrap_or_else(|err| panic!("failed to list {}: {err}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", root.display()))
            .path();
        if path.is_dir() {
            collect_rust_files(&path, paths);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            paths.push(path);
        }
    }
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_rust_files(root, &mut paths);
    paths.sort();
    paths
}

fn agent_source_files() -> Vec<PathBuf> {
    let root = manifest_root();
    let mut paths = vec![root.join("src/agent.rs")];
    paths.extend(rust_files_under(&root.join("src/agent")));

    paths.sort();
    paths
}

fn workspace_root() -> PathBuf {
    manifest_root()
        .parent()
        .and_then(|path| path.parent())
        .unwrap_or_else(|| panic!("failed to find workspace root from manifest dir"))
        .to_path_buf()
}

fn core_source_and_test_files() -> Vec<PathBuf> {
    let root = manifest_root();
    let mut paths = rust_files_under(&root.join("src"));
    paths.extend(rust_files_under(&root.join("tests")));
    paths.sort();
    paths
}

fn has_allowlist_comment(lines: &[&str], index: usize) -> bool {
    line_has_allowlist_comment(lines[index])
        || index > 0 && line_has_allowlist_comment(lines[index - 1])
}

fn line_has_allowlist_comment(line: &str) -> bool {
    let Some(reason) = line.trim_start().strip_prefix("// ") else {
        return false;
    };
    let Some(reason) = reason.strip_prefix(RAW_EXECUTE_ALLOWLIST) else {
        return false;
    };
    !reason.trim().is_empty()
}

fn is_dispatcher_execution_allowed(path: &Path) -> bool {
    let root = manifest_root();
    let relative = path
        .strip_prefix(&root)
        .unwrap_or_else(|err| panic!("failed to relativize {}: {err}", path.display()));

    relative.starts_with("src/harness") || relative.starts_with("tests")
}

#[test]
fn agent_source_does_not_call_raw_tool_execute_paths() {
    for path in agent_source_files() {
        let source = read_source(&path);
        let lines: Vec<_> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if has_allowlist_comment(&lines, index) {
                continue;
            }

            assert!(
                !line.contains(".execute(") && !line.contains("::execute("),
                "Agent-side code must not directly execute raw tools; found forbidden execute call in {}:{}: {}\nIf this is a harmless false positive, add an adjacent comment: // {RAW_EXECUTE_ALLOWLIST} <reason>",
                path.display(),
                index + 1,
                line.trim()
            );
        }
    }
}

#[test]
fn provider_tool_calls_enter_through_harness_admission() {
    let tool_execution_path = manifest_root().join("src/agent/tool_execution.rs");
    let source = read_source(&tool_execution_path);
    let lines: Vec<_> = source.lines().collect();
    let calls_harness = lines.iter().enumerate().any(|(index, line)| {
        if !line.contains(".execute_skill(") {
            return false;
        }

        let start = index.saturating_sub(3);
        lines[start..=index].iter().any(|line| {
            line.contains("self.harness")
                || line.contains("harness.execute_skill(")
                || line.contains(".harness.execute_skill(")
        })
    });

    assert!(
        calls_harness,
        "provider tool calls must enter through AgentHarness::execute_skill"
    );
    assert!(
        !source.contains("SkillDispatcher") && !source.contains("dispatcher.execute("),
        "Agent-side code must not bypass Harness admission by calling SkillDispatcher directly"
    );
}

#[test]
fn raw_skill_dispatcher_execution_stays_inside_harness_or_tests() {
    for path in core_source_and_test_files() {
        let source = read_source(&path);
        for (index, line) in source.lines().enumerate() {
            let uses_raw_dispatcher_execution =
                line.contains("dispatcher.execute(") || line.contains("SkillDispatcher::execute(");
            if !uses_raw_dispatcher_execution {
                continue;
            }

            assert!(
                is_dispatcher_execution_allowed(&path),
                "raw SkillDispatcher execution is only allowed inside harness modules or tests; found in {}:{}: {}",
                path.display(),
                index + 1,
                line.trim()
            );
        }
    }
}

#[test]
fn agent_run_loop_does_not_spawn_shell_for_task_or_probe_execution() {
    let path = manifest_root().join("src/agent/run_loop.rs");
    let source = read_source(&path);

    assert!(
        !source.contains("Command::new(\"sh\")") && !source.contains("Command::new(\"bash\")"),
        "agent run loop must not spawn shell directly; use command_availability for probes and Harness bash Skill for task execution"
    );
    assert!(
        !source.contains(".arg(\"-c\")"),
        "agent run loop must not build shell command strings directly"
    );
}

#[test]
fn workflow_verifier_owns_evidence_policy_not_agent_loop() {
    let run_loop = read_source(&manifest_root().join("src/agent/run_loop.rs"));
    let verifier = manifest_root().join("src/workflow_verification");

    assert!(
        verifier.is_dir(),
        "workflow verifier must be a module directory"
    );
    for forbidden in [
        "fn requirement_evidence_present",
        "fn verification_command_matches_requirement",
        "fn unrecovered_receipt_issues",
        "is_release_gate_verification_command",
        "web_search",
    ] {
        assert!(
            !run_loop.contains(forbidden),
            "workflow evidence policy `{forbidden}` belongs in workflow_verification, not agent/run_loop.rs"
        );
    }
}

#[test]
fn prompt_context_does_not_inject_receipts_or_transcripts_wholesale() {
    let prompt = read_source(&manifest_root().join("src/prompt.rs"));
    let prompt_context = read_source(&manifest_root().join("src/agent/prompt_context.rs"));

    assert!(
        !prompt.contains("tool_receipts") && !prompt_context.contains("tool_receipts"),
        "prompt rendering must not inject raw receipt history"
    );
    assert!(
        !prompt.contains("telegram-history") && !prompt_context.contains("telegram-history"),
        "prompt rendering must not inject raw transcript stores"
    );
    assert!(
        prompt.contains("Run Checkpoint"),
        "prompt should use compact RunCheckpoint anchors instead"
    );
}

#[test]
fn release_binary_path_keeps_ci_gate_before_packaging_build() {
    let script = read_source(&workspace_root().join("scripts/ci-gate.sh"));
    let fmt = script.find("cargo fmt --all --check").expect("fmt gate");
    let clippy = script
        .find("cargo clippy --all-targets")
        .expect("clippy gate");
    let test = script.find("cargo test --locked").expect("test gate");
    let release = script
        .find("cargo build --locked --release -p topagent-cli --bin topagent")
        .expect("release binary build");

    assert!(
        fmt < release && clippy < release && test < release,
        "release binary build must stay behind fmt, clippy, and test gates"
    );
}

#[cfg(feature = "computer-use")]
#[test]
fn computer_use_scaffold_cannot_claim_real_backend_success() {
    let source = read_source(&manifest_root().join("src/tools/computer_use.rs"));

    assert!(source.contains("was not performed"));
    assert!(source.contains("No desktop automation backend or sidecar is configured"));
    assert!(!source.contains("performed successfully"));
}

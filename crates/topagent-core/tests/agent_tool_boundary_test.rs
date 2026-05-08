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

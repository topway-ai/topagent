use std::fs;
use std::path::{Path, PathBuf};

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn agent_source_files() -> Vec<PathBuf> {
    let root = manifest_root();
    let mut paths = vec![root.join("src/agent.rs")];

    let agent_dir = root.join("src/agent");
    for entry in fs::read_dir(&agent_dir)
        .unwrap_or_else(|err| panic!("failed to list {}: {err}", agent_dir.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", agent_dir.display()))
            .path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            paths.push(path);
        }
    }

    paths.sort();
    paths
}

#[test]
fn agent_source_does_not_call_raw_tool_execute_paths() {
    for path in agent_source_files() {
        let source = read_source(&path);
        for (index, line) in source.lines().enumerate() {
            assert!(
                !line.contains(".execute(") && !line.contains("::execute("),
                "Agent-side code must not directly execute raw tools; found forbidden execute call in {}:{}: {}",
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
    let compact_source: String = source.chars().filter(|ch| !ch.is_whitespace()).collect();

    assert!(
        compact_source
            .contains("self.harness.execute_skill(&name,args.clone(),phase,ctx,&self.options)"),
        "provider tool calls must enter through AgentHarness::execute_skill"
    );
    assert!(
        !source.contains("SkillDispatcher") && !source.contains("dispatcher.execute("),
        "Agent-side code must not bypass Harness admission by calling SkillDispatcher directly"
    );
}

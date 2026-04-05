use crate::memory::WorkspaceMemory;
use std::path::PathBuf;
use topagent_core::{
    context::ExecutionContext, tools::default_tools, Agent, Message, ModelRoute,
    OpenRouterProvider, RuntimeOptions,
};
use tracing::warn;

pub(crate) fn prepare_workspace_memory(workspace_root: PathBuf) -> WorkspaceMemory {
    let memory = WorkspaceMemory::new(workspace_root);
    if let Err(err) = memory.ensure_layout() {
        warn!(
            "failed to initialize workspace memory layout in {}: {}",
            memory.workspace_root().display(),
            err
        );
    }
    memory
}

pub(crate) fn prepare_run_context(
    base_ctx: &ExecutionContext,
    memory: &WorkspaceMemory,
    instruction: &str,
    transcript_messages: Option<&[Message]>,
) -> ExecutionContext {
    let mut run_ctx = base_ctx.clone();
    if let Err(err) = memory.consolidate_memory_if_needed() {
        warn!(
            "failed to consolidate workspace memory index in {}: {}",
            memory.workspace_root().display(),
            err
        );
    }
    match memory.build_prompt(instruction, transcript_messages) {
        Ok(memory_prompt) => {
            if let Some(memory_context) = memory_prompt.prompt {
                run_ctx = run_ctx.with_memory_context(memory_context);
            }
        }
        Err(err) => {
            warn!(
                "failed to build workspace memory context in {}: {}",
                memory.workspace_root().display(),
                err
            );
        }
    }
    run_ctx
}

pub(crate) fn build_agent(route: &ModelRoute, api_key: &str, options: RuntimeOptions) -> Agent {
    let tools = default_tools();
    let provider = Box::new(OpenRouterProvider::with_tools_and_timeout(
        api_key,
        tools.specs(),
        options.provider_timeout_secs,
    ));
    Agent::with_route(provider, route.clone(), tools.into_inner(), options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_prepare_run_context_attaches_memory_prompt() {
        let temp = TempDir::new().unwrap();
        let topics_dir = temp.path().join(".topagent/topics");
        fs::create_dir_all(&topics_dir).unwrap();
        fs::write(
            temp.path().join(".topagent/MEMORY.md"),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | tags: runtime | note: runtime details\n",
        )
        .unwrap();
        fs::write(
            topics_dir.join("architecture.md"),
            "# Architecture\nruntime details",
        )
        .unwrap();

        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let run_ctx = prepare_run_context(&ctx, &memory, "inspect runtime architecture", None);

        let memory_context = run_ctx.memory_context().unwrap();
        assert!(memory_context.contains("Always-Loaded Index"));
        assert!(memory_context.contains("# Architecture"));
    }

    #[test]
    fn test_build_agent_respects_tool_authoring_option() {
        let disabled = build_agent(
            &ModelRoute::default(),
            "test-key",
            RuntimeOptions::default(),
        );
        assert!(!disabled
            .tool_specs()
            .iter()
            .any(|spec| spec.name == "create_tool"));

        let enabled = build_agent(
            &ModelRoute::default(),
            "test-key",
            RuntimeOptions::default().with_generated_tool_authoring(true),
        );
        assert!(enabled
            .tool_specs()
            .iter()
            .any(|spec| spec.name == "create_tool"));
    }
}

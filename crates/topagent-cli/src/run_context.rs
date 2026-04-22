use crate::memory::WorkspaceMemory;
use std::path::PathBuf;
use topagent_core::{
    classify_operator_instruction, context::ExecutionContext, tools::default_tools, Agent, Message,
    ModelRoute, OpenRouterProvider, RuntimeOptions,
};
use tracing::warn;

#[derive(Debug, Clone)]
pub(crate) struct PreparedRunContext {
    pub run_ctx: ExecutionContext,
    pub loaded_procedure_files: Vec<String>,
}

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
) -> PreparedRunContext {
    let mut run_ctx = base_ctx.clone();
    let mut loaded_procedure_files = Vec::new();
    let mut trust_context = classify_operator_instruction(instruction);
    if let Err(err) = memory.consolidate_memory_if_needed() {
        warn!(
            "failed to consolidate workspace memory index in {}: {}",
            memory.workspace_root().display(),
            err
        );
    }
    match memory.build_prompt(instruction, transcript_messages) {
        Ok(memory_prompt) => {
            loaded_procedure_files = memory_prompt.stats.loaded_procedure_files.clone();
            trust_context.merge(&memory_prompt.trust_context);
            if let Some(operator_context) = memory_prompt.operator_prompt {
                run_ctx = run_ctx.with_operator_context(operator_context);
            }
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
    run_ctx = run_ctx.with_run_trust_context(trust_context);
    PreparedRunContext {
        run_ctx,
        loaded_procedure_files,
    }
}

pub(crate) fn build_agent(route: &ModelRoute, api_key: &str, options: RuntimeOptions) -> Agent {
    let tools = default_tools();
    let provider = Box::new(OpenRouterProvider::with_tools_timeout_and_base_url(
        api_key,
        tools.specs(),
        options.provider_timeout_secs,
        route.provider.base_url().to_string(),
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
        let notes_dir = temp.path().join(".topagent/notes");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(
            temp.path().join(".topagent/MEMORY.md"),
            "# TopAgent Memory Index\n\n- title: architecture | file: notes/architecture.md | status: verified | tags: runtime | note: runtime details\n",
        )
        .unwrap();
        fs::write(
            notes_dir.join("architecture.md"),
            "# Architecture\nruntime details",
        )
        .unwrap();

        let ctx = ExecutionContext::new(temp.path().to_path_buf());
        let memory = WorkspaceMemory::new(temp.path().to_path_buf());
        let prepared = prepare_run_context(&ctx, &memory, "inspect runtime architecture", None);
        let run_ctx = prepared.run_ctx;

        assert!(run_ctx.operator_context().is_none());
        let memory_context = run_ctx.memory_context().unwrap();
        assert!(memory_context.contains("Always-Loaded Index"));
        assert!(memory_context.contains("# Architecture"));
        assert!(prepared.loaded_procedure_files.is_empty());
    }
}

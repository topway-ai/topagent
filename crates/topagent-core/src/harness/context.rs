use crate::capability::CapabilityProfile;
use crate::context::ExecutionContext;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBundle {
    pub workspace_root: PathBuf,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub access_profile: Option<CapabilityProfile>,
    pub has_memory_briefing: bool,
    pub has_operator_briefing: bool,
}

impl ContextBundle {
    pub fn from_execution_context(ctx: &ExecutionContext) -> Self {
        Self {
            workspace_root: ctx.workspace_root.clone(),
            task_id: ctx.task_id().map(ToString::to_string),
            session_id: ctx.session_id().map(ToString::to_string),
            access_profile: ctx
                .capability_manager()
                .map(|manager| manager.config().profile),
            has_memory_briefing: ctx.memory_context().is_some(),
            has_operator_briefing: ctx.operator_context().is_some(),
        }
    }
}

use crate::behavior::{BehaviorContract, BehaviorPromptContext, NO_PROJECT_INSTRUCTIONS_NOTE};
use crate::tool_spec::ToolSpec;

pub const NO_PI_MD_NOTE: &str = NO_PROJECT_INSTRUCTIONS_NOTE;

pub fn build_system_prompt(tools: &[ToolSpec], external_tools: &[ToolSpec]) -> String {
    BehaviorContract::default().render_system_prompt(&BehaviorPromptContext {
        available_tools: tools,
        external_tools,
        project_instructions: None,
        memory_context: None,
        current_plan: None,
        run_state: None,
        generated_tool_warnings: &[],
        planning_required_now: false,
        approval_mailbox_available: false,
    })
}

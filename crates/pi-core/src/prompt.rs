use crate::tool_spec::ToolSpec;

pub fn build_system_prompt(tools: &[ToolSpec]) -> String {
    let mut prompt = String::from("You are a coding assistant. You have access to tools:\n\n");
    for tool in tools {
        prompt.push_str(&format!("- {}: {}\n", tool.name, tool.description));
    }
    prompt.push_str("\nUse tools when needed to accomplish tasks.\n");
    prompt
}

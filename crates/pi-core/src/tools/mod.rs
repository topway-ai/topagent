mod bash;
mod edit;
mod read;
mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use read::ReadTool;
pub use write::WriteTool;

use crate::{tool_spec::ToolSpec, Result};
use std::collections::HashMap;

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn spec(&self) -> ToolSpec;
    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &crate::context::ExecutionContext,
    ) -> Result<String>;
}

pub fn make_tools(ctx: &crate::context::ExecutionContext) -> HashMap<String, Box<dyn Tool>> {
    let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
    tools.insert(
        "read".into(),
        Box::new(ReadTool::new(ctx.clone())) as Box<dyn Tool>,
    );
    tools.insert(
        "write".into(),
        Box::new(WriteTool::new(ctx.clone())) as Box<dyn Tool>,
    );
    tools.insert(
        "edit".into(),
        Box::new(EditTool::new(ctx.clone())) as Box<dyn Tool>,
    );
    tools.insert("bash".into(), Box::new(BashTool::new()) as Box<dyn Tool>);
    tools
}

pub fn all_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::read(),
        ToolSpec::write(),
        ToolSpec::edit(),
        ToolSpec::bash(),
    ]
}

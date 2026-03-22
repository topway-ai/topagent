mod bash;
mod edit;
mod read;
mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use read::ReadTool;
pub use write::WriteTool;

use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::Result;
use std::collections::HashMap;

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    by_name: HashMap<String, usize>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn add(&mut self, tool: Box<dyn Tool>) {
        let name = tool.spec().name.to_string();
        if !self.by_name.contains_key(&name) {
            let idx = self.tools.len();
            self.by_name.insert(name, idx);
            self.tools.push(tool);
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.by_name
            .get(name)
            .and_then(|&idx| self.tools.get(idx).map(|t| t.as_ref()))
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn into_inner(self) -> Vec<Box<dyn Tool>> {
        self.tools
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.add(Box::new(ReadTool::new()));
    registry.add(Box::new(WriteTool::new()));
    registry.add(Box::new(EditTool::new()));
    registry.add(Box::new(BashTool::new()));
    registry
}

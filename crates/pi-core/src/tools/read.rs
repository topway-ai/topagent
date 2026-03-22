use crate::context::ExecutionContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadArgs {
    pub path: String,
}

#[derive(Clone)]
pub struct ReadTool {
    _ctx: ExecutionContext,
}

impl ReadTool {
    pub fn new(ctx: ExecutionContext) -> Self {
        Self { _ctx: ctx }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new(ExecutionContext::new(std::path::PathBuf::from(".")))
    }
}

impl crate::tools::Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "read file contents"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::read()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ExecutionContext) -> Result<String> {
        let args: ReadArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path(&args.path)?;
        std::fs::read_to_string(&full_path).map_err(|e| {
            Error::ToolFailed(format!("failed to read {}: {}", full_path.display(), e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ExecutionContext;
    use crate::tools::Tool;
    use std::fs;
    use tempfile::TempDir;

    fn test_ctx() -> (ExecutionContext, TempDir) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        (ExecutionContext::new(root), temp)
    }

    #[test]
    fn test_read_file_inside_workspace() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new(ctx.clone());
        fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(serde_json::json!({"path": "test.txt"}), &ctx);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_read_path_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new(ctx.clone());
        let result = tool.execute(serde_json::json!({"path": "../etc/passwd"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_nested_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new(ctx.clone());
        let result = tool.execute(serde_json::json!({"path": "a/../../b"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_absolute_path_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new(ctx.clone());
        let result = tool.execute(serde_json::json!({"path": "/etc/passwd"}), &ctx);
        assert!(result.is_err());
    }
}

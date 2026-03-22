use crate::context::ExecutionContext;
use crate::file_util::atomic_write;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Clone)]
pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::write()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ExecutionContext) -> Result<String> {
        let args: WriteArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path(&args.path)?;
        atomic_write(&full_path, &args.content)?;
        Ok(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            full_path.display()
        ))
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
    fn test_write_file_inside_workspace() {
        let (ctx, _temp) = test_ctx();
        let tool = WriteTool::new();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "content": "hello world"}),
            &ctx,
        );
        assert!(result.is_ok(), "{:?}", result);
        let content = fs::read_to_string(ctx.resolve_path("test.txt").unwrap()).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_write_nested_path() {
        let (ctx, _temp) = test_ctx();
        let tool = WriteTool::new();
        let result = tool.execute(
            serde_json::json!({"path": "a/b/c.txt", "content": "nested"}),
            &ctx,
        );
        assert!(result.is_ok(), "{:?}", result);
        let content = fs::read_to_string(ctx.resolve_path("a/b/c.txt").unwrap()).unwrap();
        assert_eq!(content, "nested");
    }

    #[test]
    fn test_write_path_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = WriteTool::new();
        let result = tool.execute(
            serde_json::json!({"path": "../test.txt", "content": "bad"}),
            &ctx,
        );
        assert!(result.is_err());
    }
}

use crate::context::ExecutionContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditArgs {
    pub path: String,
    pub find: String,
    pub replace: String,
}

#[derive(Clone)]
pub struct EditTool {
    _ctx: ExecutionContext,
}

impl EditTool {
    pub fn new(ctx: ExecutionContext) -> Self {
        Self { _ctx: ctx }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new(ExecutionContext::new(std::path::PathBuf::from(".")))
    }
}

impl crate::tools::Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "replace first occurrence of find string with replace"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::edit()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ExecutionContext) -> Result<String> {
        let args: EditArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path(&args.path)?;
        let content = std::fs::read_to_string(&full_path).map_err(|e| {
            Error::ToolFailed(format!("failed to read {}: {}", full_path.display(), e))
        })?;

        if !content.contains(&args.find) {
            return Err(Error::ToolFailed(format!(
                "string '{}' not found in {}",
                args.find,
                full_path.display()
            )));
        }

        let new_content = content.replacen(&args.find, &args.replace, 1);
        std::fs::write(&full_path, &new_content).map_err(|e| {
            Error::ToolFailed(format!("failed to write {}: {}", full_path.display(), e))
        })?;

        Ok(format!("replaced 1 occurrence in {}", full_path.display()))
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
    fn test_edit_file() {
        let (ctx, _temp) = test_ctx();
        let tool = EditTool::new(ctx.clone());
        fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "find": "world", "replace": "rust"}),
            &ctx,
        );
        assert!(result.is_ok(), "{:?}", result);
        let content = fs::read_to_string(ctx.resolve_path("test.txt").unwrap()).unwrap();
        assert_eq!(content, "hello rust");
    }

    #[test]
    fn test_edit_not_found() {
        let (ctx, _temp) = test_ctx();
        let tool = EditTool::new(ctx.clone());
        fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "find": "nonexistent", "replace": "replacement"}),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_edit_path_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = EditTool::new(ctx.clone());
        let result = tool.execute(
            serde_json::json!({"path": "../test.txt", "find": "a", "replace": "b"}),
            &ctx,
        );
        assert!(result.is_err());
    }
}

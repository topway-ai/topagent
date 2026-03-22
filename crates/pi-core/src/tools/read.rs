use crate::context::ExecutionContext;
use crate::file_util::read_text_file;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadArgs {
    pub path: String,
}

#[derive(Clone)]
pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ExecutionContext) -> Result<String> {
        let args: ReadArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path(&args.path)?;
        read_text_file(&full_path)
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
        let tool = ReadTool::new();
        fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(serde_json::json!({"path": "test.txt"}), &ctx);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_read_path_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "../etc/passwd"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_nested_traversal_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "a/../../b"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_absolute_path_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "/etc/passwd"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_binary_file_rejected() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        fs::write(
            ctx.resolve_path("binary.bin").unwrap(),
            b"\x00\x01\x02binary",
        )
        .unwrap();
        let result = tool.execute(serde_json::json!({"path": "binary.bin"}), &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("binary"), "expected binary rejection: {}", err);
    }

    #[test]
    fn test_read_truncation() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        let large_content = "x".repeat(100 * 1024);
        fs::write(ctx.resolve_path("large.txt").unwrap(), &large_content).unwrap();
        let result = tool.execute(serde_json::json!({"path": "large.txt"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("truncated"),
            "expected truncation notice: {}",
            output
        );
        assert!(
            output.contains("102400"),
            "expected original size: {}",
            output
        );
    }

    #[test]
    fn test_read_truncation_preserves_utf8_boundary() {
        let (ctx, _temp) = test_ctx();
        let tool = ReadTool::new();
        let emoji = "\u{1F600}"; // 4-byte UTF-8 character
        let repeated = emoji.repeat(20_000); // creates 80KB of content (4 bytes each)
        fs::write(ctx.resolve_path("emoji.txt").unwrap(), &repeated).unwrap();
        let result = tool.execute(serde_json::json!({"path": "emoji.txt"}), &ctx);
        assert!(result.is_ok(), "{:?}", result);
        let output = result.unwrap();
        assert!(
            output.contains("truncated"),
            "expected truncation: {}",
            output
        );
        assert!(
            String::from_utf8(
                output
                    .lines()
                    .find(|l| l.starts_with("[ReadTool] File truncated"))
                    .map(|l| l.to_string())
                    .unwrap_or_default()
                    .into_bytes()
            )
            .is_ok(),
            "truncation should be on valid UTF-8 boundary"
        );
    }
}

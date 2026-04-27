use crate::capability::AccessMode;
use crate::context::ToolContext;
use crate::file_util::read_text_file_with_limit;
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

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: ReadArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path_for_access(
            &args.path,
            AccessMode::Read,
            "read the file requested by the operator",
        )?;
        read_text_file_with_limit(&full_path, ctx.runtime.max_read_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_read_file_inside_workspace() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = ReadTool::new();
        fs::write(ctx.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(serde_json::json!({"path": "test.txt"}), &ctx);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_read_path_traversal_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "../etc/passwd"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_nested_traversal_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "a/../../b"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_absolute_path_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "/etc/passwd"}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_binary_file_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
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
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
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
    fn test_read_custom_max_bytes() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default().with_max_read_bytes(100);
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = ReadTool::new();
        let content = "x".repeat(200);
        fs::write(ctx.resolve_path("test.txt").unwrap(), &content).unwrap();
        let result = tool.execute(serde_json::json!({"path": "test.txt"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("truncated"),
            "expected truncation: {}",
            output
        );
        assert!(output.contains("200"), "expected original size: {}", output);
    }

    #[test]
    fn test_read_truncation_preserves_utf8_boundary() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
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
        let truncated_marker = "[ReadTool] File truncated:";
        if let Some(start_idx) = output.find(truncated_marker) {
            let after_marker = &output[start_idx..];
            if let Some(end_idx) = after_marker.find('\n') {
                let content_section = &after_marker[..end_idx];
                assert!(
                    String::from_utf8(content_section.to_string().into_bytes()).is_ok(),
                    "truncation marker line should be valid UTF-8: {}",
                    content_section
                );
            }
        }
        for (i, line) in output.lines().enumerate() {
            if i > 0 && line.starts_with("[ReadTool]") {
                continue;
            }
            if !line.is_empty() {
                assert!(
                    std::str::from_utf8(line.as_bytes()).is_ok(),
                    "non-marker content line should be valid UTF-8: {:?}",
                    line
                );
            }
        }
    }
}

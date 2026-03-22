use crate::context::ToolContext;
use crate::file_util::{atomic_write, read_text_file};
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditArgs {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Clone)]
pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::edit()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: EditArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.exec.resolve_path(&args.path)?;
        let content = read_text_file(&full_path)?;

        let matches: Vec<usize> = content
            .match_indices(&args.old_text)
            .map(|(i, _)| i)
            .collect();

        if matches.is_empty() {
            return Err(Error::EditFailed(format!(
                "text '{}' not found in {}",
                args.old_text,
                full_path.display()
            )));
        }

        let count = if args.replace_all {
            let new_content = content.replace(&args.old_text, &args.new_text);
            atomic_write(&full_path, &new_content)?;
            matches.len()
        } else {
            if matches.len() > 1 {
                return Err(Error::EditFailed(format!(
                    "ambiguous edit: '{}' occurs {} times in {}, use replace_all to replace all occurrences",
                    args.old_text,
                    matches.len(),
                    full_path.display()
                )));
            }
            let new_content = content.replacen(&args.old_text, &args.new_text, 1);
            atomic_write(&full_path, &new_content)?;
            1
        };

        Ok(format!(
            "replaced {} occurrence(s) in {}",
            count,
            full_path.display()
        ))
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
    fn test_edit_unique_match() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(ctx.exec.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "old_text": "world", "new_text": "rust"}),
            &ctx,
        );
        assert!(result.is_ok(), "{:?}", result);
        let content = fs::read_to_string(ctx.exec.resolve_path("test.txt").unwrap()).unwrap();
        assert_eq!(content, "hello rust");
    }

    #[test]
    fn test_edit_not_found() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(ctx.exec.resolve_path("test.txt").unwrap(), "hello world").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "old_text": "nonexistent", "new_text": "replacement"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "expected not found error: {}",
            err
        );
    }

    #[test]
    fn test_edit_ambiguous_without_replace_all() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(ctx.exec.resolve_path("test.txt").unwrap(), "foo bar foo").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "old_text": "foo", "new_text": "baz"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ambiguous"),
            "expected ambiguous error: {}",
            err
        );
    }

    #[test]
    fn test_edit_replace_all() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(ctx.exec.resolve_path("test.txt").unwrap(), "foo bar foo").unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "old_text": "foo", "new_text": "baz", "replace_all": true}),
            &ctx,
        );
        assert!(result.is_ok(), "{:?}", result);
        let content = fs::read_to_string(ctx.exec.resolve_path("test.txt").unwrap()).unwrap();
        assert_eq!(content, "baz bar baz");
    }

    #[test]
    fn test_edit_path_traversal_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        let result = tool.execute(
            serde_json::json!({"path": "../test.txt", "old_text": "a", "new_text": "b"}),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_edit_deterministic_single_replacement() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(
            ctx.exec.resolve_path("test.txt").unwrap(),
            "line1\nfoo\nline3\nfoo\nline5",
        )
        .unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt", "old_text": "foo", "new_text": "bar"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ambiguous"),
            "expected ambiguous error: {}",
            err
        );
    }

    #[test]
    fn test_edit_binary_file_rejected() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = EditTool::new();
        fs::write(
            ctx.exec.resolve_path("binary.bin").unwrap(),
            b"\x00\x01binary",
        )
        .unwrap();
        let result = tool.execute(
            serde_json::json!({"path": "binary.bin", "old_text": "a", "new_text": "b"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("binary"), "expected binary rejection: {}", err);
    }
}

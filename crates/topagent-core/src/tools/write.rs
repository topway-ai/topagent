use crate::capability::AccessMode;
use crate::context::ToolContext;
use crate::file_util::atomic_write;
use crate::run_snapshot::{RunSnapshotCaptureMetadata, RunSnapshotCaptureSource};
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

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: WriteArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let full_path = ctx.resolve_path_for_access(
            &args.path,
            AccessMode::Write,
            "write the file requested by the operator",
        )?;
        if let Some(run_snapshot_store) = ctx.run_snapshot_store() {
            run_snapshot_store.capture_file(
                &args.path,
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Write,
                    "structured write",
                ),
            )?;
        }
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
    use crate::context::{ExecutionContext, ToolContext};
    use crate::run_snapshot::WorkspaceRunSnapshotStore;
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_write_file_inside_workspace() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
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
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
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
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = WriteTool::new();
        let result = tool.execute(
            serde_json::json!({"path": "../test.txt", "content": "bad"}),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_write_captures_preexisting_file_for_run_snapshot_restore() {
        let temp = TempDir::new().unwrap();
        let original_path = temp.path().join("test.txt");
        fs::write(&original_path, "before").unwrap();

        let exec = ExecutionContext::new(temp.path().to_path_buf())
            .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                temp.path().to_path_buf(),
            ));
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = WriteTool::new();

        tool.execute(
            serde_json::json!({"path": "test.txt", "content": "after"}),
            &ctx,
        )
        .unwrap();

        exec.run_snapshot_store()
            .unwrap()
            .restore_latest()
            .unwrap()
            .unwrap();
        let content = fs::read_to_string(original_path).unwrap();
        assert_eq!(content, "before");
    }
}

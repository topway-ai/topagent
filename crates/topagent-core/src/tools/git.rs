use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiffArgs {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone)]
pub struct GitStatusTool;

impl GitStatusTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".to_string(),
            description: "show git repository status".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn execute(&self, _args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let output = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&ctx.exec.workspace_root)
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git status: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!("git status failed: {}", stderr)));
        }

        if stdout.is_empty() {
            return Ok("git repository is clean (no changes)".to_string());
        }

        Ok(format!("git status:\n{}", stdout))
    }
}

#[derive(Clone)]
pub struct GitDiffTool;

impl GitDiffTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitDiffTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitDiffTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".to_string(),
            description: "show git diff of changes (optionally for a specific path)".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "optional relative path to diff"}
                },
                "required": []
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: GitDiffArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;

        let mut cmd = Command::new("git");
        cmd.args(["diff"]);

        if let Some(path) = &args.path {
            let full_path = ctx.exec.resolve_path(path)?;
            cmd.arg(full_path);
        }

        cmd.current_dir(&ctx.exec.workspace_root);

        let output = cmd
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git diff: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!("git diff failed: {}", stderr)));
        }

        if stdout.is_empty() {
            return Ok("no changes to diff".to_string());
        }

        let truncated = truncate_git_output(&stdout, ctx.runtime.max_bash_output_bytes);
        Ok(format!("git diff:\n{}", truncated))
    }
}

#[derive(Clone)]
pub struct GitBranchTool;

impl GitBranchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitBranchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitBranchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_branch".to_string(),
            description: "show current git branch name".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn execute(&self, _args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&ctx.exec.workspace_root)
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git branch: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!("git branch failed: {}", stderr)));
        }

        let branch = stdout.trim();
        if branch.is_empty() {
            return Ok("not on any branch (possibly detached HEAD)".to_string());
        }

        Ok(format!("current branch: {}", branch))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCloneArgs {
    pub url: String,
    pub directory: Option<String>,
}

#[derive(Clone)]
pub struct GitCloneTool;

impl GitCloneTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitCloneTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitCloneTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_clone".to_string(),
            description: "clone a git repository into the workspace (requires approval)"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "git repository URL to clone"},
                    "directory": {"type": "string", "description": "optional target directory name (defaults to repo name)"}
                },
                "required": ["url"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: GitCloneArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;

        let target_dir = if let Some(dir) = args.directory {
            ctx.exec.workspace_root.join(dir)
        } else {
            let repo_name = args
                .url
                .trim_end_matches(".git")
                .split('/')
                .last()
                .unwrap_or("repo");
            ctx.exec.workspace_root.join(repo_name)
        };

        if target_dir.exists() {
            return Err(Error::ToolFailed(format!(
                "directory already exists: {}",
                target_dir.display()
            )));
        }

        let mut cmd = Command::new("git");
        cmd.args(["clone", &args.url])
            .arg(&target_dir)
            .current_dir(&ctx.exec.workspace_root);

        let output = cmd
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git clone: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!("git clone failed: {}", stderr)));
        }

        Ok(format!(
            "cloned repository to {}",
            target_dir.file_name().unwrap_or_default().to_string_lossy()
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitAddArgs {
    pub paths: Vec<String>,
}

#[derive(Clone)]
pub struct GitAddTool;

impl GitAddTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitAddTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitAddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_add".to_string(),
            description: "stage files in git (add to index)".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "relative paths of files to stage"
                    }
                },
                "required": ["paths"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: GitAddArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;

        if args.paths.is_empty() {
            return Err(Error::InvalidInput(
                "at least one path required".to_string(),
            ));
        }

        let mut cmd = Command::new("git");
        cmd.args(["add"]);

        for path in &args.paths {
            let full_path = ctx.exec.resolve_path(path)?;
            cmd.arg(full_path);
        }

        cmd.current_dir(&ctx.exec.workspace_root);

        let output = cmd
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git add: {}", e)))?;

        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!("git add failed: {}", stderr)));
        }

        Ok(format!("staged {} file(s)", args.paths.len()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitArgs {
    pub message: String,
}

#[derive(Clone)]
pub struct GitCommitTool;

impl GitCommitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitCommitTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for GitCommitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_commit".to_string(),
            description: "commit staged changes in git with a message".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "commit message"
                    }
                },
                "required": ["message"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: GitCommitArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;

        if args.message.trim().is_empty() {
            return Err(Error::InvalidInput(
                "commit message cannot be empty".to_string(),
            ));
        }

        let output = Command::new("git")
            .args(["commit", "-m", &args.message])
            .current_dir(&ctx.exec.workspace_root)
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute git commit: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::ToolFailed(format!(
                "git commit failed: {}",
                if stderr.is_empty() { stdout } else { stderr }
            )));
        }

        if stdout.is_empty() {
            Ok("committed successfully".to_string())
        } else {
            Ok(stdout.to_string())
        }
    }
}

fn truncate_git_output(output: &str, max_size: usize) -> String {
    if output.len() <= max_size {
        return output.to_string();
    }
    format!(
        "{}...\n[Output truncated: {} bytes total, showing first {}]",
        &output[..max_size],
        output.len(),
        max_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use tempfile::TempDir;

    #[test]
    fn test_git_status_clean_repo() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitStatusTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("clean") || output.contains("git status"));
    }

    #[test]
    fn test_git_branch() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitBranchTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("branch"));
    }

    #[test]
    fn test_git_diff_no_changes() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitDiffTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("no changes") || output.contains("git diff"));
    }

    #[test]
    fn test_git_status_not_a_repo() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let tool = GitStatusTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_git_add_stages_file() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();

        let tool = GitAddTool::new();
        let result = tool.execute(serde_json::json!({"paths": ["test.txt"]}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("staged"));
    }

    #[test]
    fn test_git_add_empty_paths_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitAddTool::new();
        let result = tool.execute(serde_json::json!({"paths": []}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_git_commit_commits_staged() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        std::fs::write(temp.path().join("test.txt"), "hello").unwrap();

        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitCommitTool::new();
        let result = tool.execute(serde_json::json!({"message": "initial commit"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("committed") || output.contains("initial commit"));
    }

    #[test]
    fn test_git_commit_empty_message_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .unwrap();

        let tool = GitCommitTool::new();
        let result = tool.execute(serde_json::json!({"message": ""}), &ctx);
        assert!(result.is_err());
    }
}

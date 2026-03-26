use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::process::{Command, Output};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashArgs {
    pub command: String,
}

#[derive(Clone)]
pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::bash()
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: BashArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let output = Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .current_dir(&ctx.exec.workspace_root)
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute command: {}", e)))?;
        format_output_with_limit(output, ctx.runtime.max_bash_output_bytes)
    }
}

fn format_output_with_limit(output: Output, max_size: usize) -> Result<String> {
    let stdout_raw = &output.stdout;
    let stderr_raw = &output.stderr;
    let status = output.status;

    let stdout_len = stdout_raw.len();
    let stderr_len = stderr_raw.len();

    let mut stdout_truncated = false;
    let mut stderr_truncated = false;

    let stdout_bytes = if stdout_len > max_size {
        stdout_truncated = true;
        &stdout_raw[..max_size]
    } else {
        stdout_raw.as_slice()
    };

    let stderr_bytes = if stderr_len > max_size {
        stderr_truncated = true;
        &stderr_raw[..max_size]
    } else {
        stderr_raw.as_slice()
    };

    let stdout = String::from_utf8_lossy(stdout_bytes);
    let stderr = String::from_utf8_lossy(stderr_bytes);

    let mut result = String::new();
    if !stdout_raw.is_empty() {
        result.push_str("Output: ");
        result.push_str(&stdout);
        if stdout_truncated {
            result.push_str(&format!(
                "\n[Output truncated: {} bytes total, showing first {}]",
                stdout_len, max_size
            ));
        }
        result.push('\n');
    }
    if !stderr_raw.is_empty() {
        result.push_str("Stderr: ");
        result.push_str(&stderr);
        if stderr_truncated {
            result.push_str(&format!(
                "\n[Stderr truncated: {} bytes total, showing first {}]",
                stderr_len, max_size
            ));
        }
        result.push('\n');
    }
    result.push_str(&format!("Exit code: {}", status));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use tempfile::TempDir;

    #[test]
    fn test_bash_echo() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({"command": "echo hello"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[test]
    fn test_bash_exit_code() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({"command": "exit 1"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("exit status:"));
    }

    #[test]
    fn test_bash_respects_workspace_root() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({"command": "pwd"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains(&temp.path().to_string_lossy().to_string()));
    }

    #[test]
    fn test_bash_output_not_truncated_for_small_output() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();
        let result = tool.execute(serde_json::json!({"command": "echo 'short output'"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            !output.contains("truncated"),
            "small output should not be truncated: {}",
            output
        );
    }

    #[test]
    fn test_bash_output_truncation_respects_runtime_limit() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default().with_max_bash_output_bytes(10);
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();
        let result = tool.execute(
            serde_json::json!({"command": "echo 'this is a longer output'"}),
            &ctx,
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("truncated"),
            "output should be truncated: {}",
            output
        );
    }
}

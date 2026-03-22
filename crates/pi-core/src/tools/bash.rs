use crate::context::ExecutionContext;
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
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "execute bash command"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::bash()
    }

    fn execute(&self, args: serde_json::Value, _ctx: &ExecutionContext) -> Result<String> {
        let args: BashArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let output = Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .output()
            .map_err(|e| Error::ToolFailed(format!("failed to execute command: {}", e)))?;
        format_output(output)
    }
}

fn format_output(output: Output) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = output.status;
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str("stdout:\n");
        result.push_str(&stdout);
        result.push('\n');
    }
    if !stderr.is_empty() {
        result.push_str("stderr:\n");
        result.push_str(&stderr);
        result.push('\n');
    }
    result.push_str(&format!("exit status: {}", status));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ExecutionContext;
    use crate::tools::Tool;

    #[test]
    fn test_bash_echo() {
        let tool = BashTool::new();
        let ctx = ExecutionContext::new(std::path::PathBuf::from("."));
        let result = tool.execute(serde_json::json!({"command": "echo hello"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[test]
    fn test_bash_exit_code() {
        let tool = BashTool::new();
        let ctx = ExecutionContext::new(std::path::PathBuf::from("."));
        let result = tool.execute(serde_json::json!({"command": "exit 1"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("exit status:"));
    }
}

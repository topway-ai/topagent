use crate::context::ToolContext;
use crate::secrets;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use tracing::warn;

/// Paths the sandbox allows read-only access to.
const BWRAP_RO_BIND_CANDIDATES: &[&str] = &[
    "/usr",
    "/bin",
    "/lib",
    "/lib64",
    "/etc",
    "/nix",
    "/run/current-system",
];

/// Cached bwrap probe result: availability flag + which bind paths exist.
struct BwrapProbe {
    available: bool,
    ro_binds: Vec<&'static str>,
}

static BWRAP_PROBE: OnceLock<BwrapProbe> = OnceLock::new();

fn bwrap_probe() -> &'static BwrapProbe {
    BWRAP_PROBE.get_or_init(|| {
        let available = Command::new("bwrap")
            .args([
                "--ro-bind",
                "/usr",
                "/usr",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !available {
            warn!(
                "bwrap unavailable (not installed or user namespaces restricted); \
                 bash commands will run without filesystem sandboxing"
            );
        }
        let ro_binds = BWRAP_RO_BIND_CANDIDATES
            .iter()
            .copied()
            .filter(|p| std::path::Path::new(p).exists())
            .collect();
        BwrapProbe {
            available,
            ro_binds,
        }
    })
}

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
        if ctx.exec.is_cancelled() {
            return Err(Error::Stopped("user requested stop".into()));
        }

        // Block commands that attempt to access secrets.
        if let Some(block_msg) = secrets::check_bash_secret_access(&args.command) {
            return Ok(block_msg);
        }

        let workspace = ctx.exec.workspace_root.to_string_lossy();
        let probe = bwrap_probe();

        let mut cmd = if probe.available {
            let mut c = Command::new("bwrap");
            for path in &probe.ro_binds {
                c.args(["--ro-bind", path, path]);
            }
            // Writable workspace.
            c.args(["--bind", &workspace, &workspace]);
            // Writable /tmp.
            c.args(["--tmpfs", "/tmp"]);
            // Minimal /dev and /proc.
            c.args(["--dev", "/dev"]);
            c.args(["--proc", "/proc"]);
            // Block network access.
            c.arg("--unshare-net");
            // Set working directory inside sandbox.
            c.args(["--chdir", &workspace]);
            // The command to run inside the sandbox.
            c.args(["sh", "-c", &args.command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c")
                .arg(&args.command)
                .current_dir(&ctx.exec.workspace_root);
            c
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Strip secret-bearing environment variables from child processes.
        for var_name in secrets::SECRET_ENV_VARS {
            cmd.env_remove(var_name);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::ToolFailed(format!("failed to execute command: {}", e)))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::ToolFailed("failed to capture stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::ToolFailed("failed to capture stderr".into()))?;

        let stdout_reader = thread::spawn(move || {
            let mut stdout = stdout;
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let stderr_reader = thread::spawn(move || {
            let mut stderr = stderr;
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });

        let status = loop {
            if ctx.exec.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Error::Stopped("user requested stop".into()));
            }

            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(Error::ToolFailed(format!(
                        "failed while waiting for command: {}",
                        e
                    )));
                }
            }
        };

        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        let output = Output {
            status,
            stdout,
            stderr,
        };
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
    result.push_str(&format!("Exit code: {}", status.code().unwrap_or(-1)));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use crate::CancellationToken;
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
        assert!(result.unwrap().contains("Exit code: 1"));
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

    #[test]
    fn test_bash_can_be_cancelled() {
        let temp = TempDir::new().unwrap();
        let cancel = CancellationToken::new();
        let exec =
            ExecutionContext::new(temp.path().to_path_buf()).with_cancel_token(cancel.clone());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();

        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });

        let result = tool.execute(serde_json::json!({"command": "sleep 5"}), &ctx);
        canceller.join().unwrap();

        assert!(matches!(result, Err(Error::Stopped(_))));
    }
}

use crate::{Error, Result, cancel::CancellationToken, secrets::SECRET_ENV_VARS};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
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
                 workspace-sandboxed commands will run without filesystem sandboxing"
            );
        }
        let ro_binds = BWRAP_RO_BIND_CANDIDATES
            .iter()
            .copied()
            .filter(|path| Path::new(path).exists())
            .collect();
        BwrapProbe {
            available,
            ro_binds,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandSandboxPolicy {
    #[default]
    Host,
    Workspace,
}

impl CommandSandboxPolicy {
    pub fn description_suffix(self) -> &'static str {
        match self {
            CommandSandboxPolicy::Host => "host execution; no workspace sandbox",
            CommandSandboxPolicy::Workspace => {
                "sandboxed workspace; commands have no outbound network access"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandPlan {
    program: String,
    args: Vec<String>,
    current_dir: PathBuf,
}

fn build_command_plan(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    sandbox: CommandSandboxPolicy,
    probe: &BwrapProbe,
) -> CommandPlan {
    let workspace = workspace_root.display().to_string();
    match sandbox {
        CommandSandboxPolicy::Host => CommandPlan {
            program: program.to_string(),
            args: args.to_vec(),
            current_dir: workspace_root.to_path_buf(),
        },
        CommandSandboxPolicy::Workspace if probe.available => {
            let mut bwrap_args = Vec::new();
            for path in &probe.ro_binds {
                bwrap_args.push("--ro-bind".to_string());
                bwrap_args.push((*path).to_string());
                bwrap_args.push((*path).to_string());
            }
            bwrap_args.extend([
                "--bind".to_string(),
                workspace.clone(),
                workspace.clone(),
                "--tmpfs".to_string(),
                "/tmp".to_string(),
                "--dev".to_string(),
                "/dev".to_string(),
                "--proc".to_string(),
                "/proc".to_string(),
                "--unshare-net".to_string(),
                "--chdir".to_string(),
                workspace,
                program.to_string(),
            ]);
            bwrap_args.extend(args.iter().cloned());

            CommandPlan {
                program: "bwrap".to_string(),
                args: bwrap_args,
                current_dir: workspace_root.to_path_buf(),
            }
        }
        CommandSandboxPolicy::Workspace => CommandPlan {
            program: program.to_string(),
            args: args.to_vec(),
            current_dir: workspace_root.to_path_buf(),
        },
    }
}

pub fn run_command(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    cancel: Option<&CancellationToken>,
    sandbox: CommandSandboxPolicy,
    display_name: &str,
) -> Result<Output> {
    let plan = build_command_plan(program, args, workspace_root, sandbox, bwrap_probe());
    let mut cmd = Command::new(&plan.program);
    cmd.current_dir(&plan.current_dir);
    for arg in plan.args {
        cmd.arg(arg);
    }
    for var_name in SECRET_ENV_VARS {
        cmd.env_remove(var_name);
    }
    run_command_with_cancellation(&mut cmd, cancel, display_name)
}

fn run_command_with_cancellation(
    cmd: &mut Command,
    cancel: Option<&CancellationToken>,
    display_name: &str,
) -> Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::ToolFailed(format!("failed to execute {}: {}", display_name, e)))?;

    let stdout = child.stdout.take().ok_or_else(|| {
        Error::ToolFailed(format!("failed to capture stdout for {}", display_name))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        Error::ToolFailed(format!("failed to capture stderr for {}", display_name))
    })?;

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
        if cancel.is_some_and(|token| token.is_cancelled()) {
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
                    "failed while waiting for {}: {}",
                    display_name, e
                )));
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_probe(available: bool) -> BwrapProbe {
        BwrapProbe {
            available,
            ro_binds: vec!["/usr", "/bin"],
        }
    }

    #[test]
    fn test_workspace_sandbox_plan_uses_bwrap_when_available() {
        let temp = TempDir::new().unwrap();
        let args = vec!["-c".to_string(), "echo hi".to_string()];
        let plan = build_command_plan(
            "sh",
            &args,
            temp.path(),
            CommandSandboxPolicy::Workspace,
            &fake_probe(true),
        );

        assert_eq!(plan.program, "bwrap");
        assert!(plan.args.contains(&"--unshare-net".to_string()));
        assert!(plan.args.contains(&"--chdir".to_string()));
        assert!(plan.args.contains(&"sh".to_string()));
    }

    #[test]
    fn test_workspace_sandbox_plan_falls_back_when_bwrap_unavailable() {
        let temp = TempDir::new().unwrap();
        let args = vec!["status".to_string()];
        let plan = build_command_plan(
            "git",
            &args,
            temp.path(),
            CommandSandboxPolicy::Workspace,
            &fake_probe(false),
        );

        assert_eq!(plan.program, "git");
        assert_eq!(plan.args, args);
        assert_eq!(plan.current_dir, temp.path());
    }

    #[test]
    fn test_host_policy_uses_direct_command() {
        let temp = TempDir::new().unwrap();
        let args = vec!["TODO".to_string(), "src".to_string()];
        let plan = build_command_plan(
            "rg",
            &args,
            temp.path(),
            CommandSandboxPolicy::Host,
            &fake_probe(true),
        );

        assert_eq!(plan.program, "rg");
        assert_eq!(plan.args, args);
        assert_eq!(plan.current_dir, temp.path());
    }
}

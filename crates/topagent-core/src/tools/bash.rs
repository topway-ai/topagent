use crate::command_exec::{run_command, CommandSandboxPolicy};
use crate::context::ToolContext;
use crate::file_util::format_command_output_with_limit;
use crate::run_snapshot::{
    RunSnapshotCaptureMetadata, RunSnapshotCaptureSource, WorkspaceRunSnapshotStore,
};
use crate::secrets;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashArgs {
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BashRunSnapshotScope {
    Paths(Vec<String>),
    WorkspaceTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BashRunSnapshotReason {
    FileWriteRedirection,
    Delete,
    MoveOrRename,
    Copy,
    DirectoryMutation,
    Touch,
    Truncate,
    InPlaceEdit,
    PermissionChange,
    OwnershipChange,
    ArchiveExtraction,
    GitWorkspaceRewrite,
}

impl BashRunSnapshotReason {
    fn label(self) -> &'static str {
        match self {
            Self::FileWriteRedirection => "shell redirection write",
            Self::Delete => "shell deletion",
            Self::MoveOrRename => "shell move or rename",
            Self::Copy => "shell copy or link",
            Self::DirectoryMutation => "shell directory mutation",
            Self::Touch => "shell touch mutation",
            Self::Truncate => "shell truncate mutation",
            Self::InPlaceEdit => "shell in-place edit",
            Self::PermissionChange => "shell permission change",
            Self::OwnershipChange => "shell ownership change",
            Self::ArchiveExtraction => "shell archive extraction",
            Self::GitWorkspaceRewrite => "git workspace rewrite",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BashRunSnapshotPlan {
    reason: BashRunSnapshotReason,
    detail: String,
    scope: BashRunSnapshotScope,
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

        if let Some(run_snapshot_store) = ctx.exec.run_snapshot_store() {
            if let Some(plan) = run_snapshot_plan(&args.command) {
                capture_risky_shell_run_snapshot(run_snapshot_store, &plan, ctx)?;
            }
        }

        let output = run_command(
            "sh",
            &["-c".to_string(), args.command],
            &ctx.exec.workspace_root,
            ctx.exec.cancel_token(),
            CommandSandboxPolicy::Workspace,
            "command",
        )?;
        Ok(format_command_output_with_limit(
            output,
            ctx.runtime.max_bash_output_bytes,
        ))
    }
}

fn capture_risky_shell_run_snapshot(
    run_snapshot_store: &WorkspaceRunSnapshotStore,
    plan: &BashRunSnapshotPlan,
    ctx: &ToolContext,
) -> Result<()> {
    let metadata =
        RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Bash, plan.reason.label())
            .with_detail(plan.detail.clone());

    match &plan.scope {
        BashRunSnapshotScope::WorkspaceTree => {
            run_snapshot_store.capture_workspace(metadata)?;
        }
        BashRunSnapshotScope::Paths(paths) => {
            let normalized = normalize_run_snapshot_paths(paths, ctx);
            if normalized.is_empty() {
                return Ok(());
            }
            run_snapshot_store.capture_paths(&normalized, metadata)?;
        }
    }

    Ok(())
}

fn normalize_run_snapshot_paths(paths: &[String], ctx: &ToolContext) -> Vec<String> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if path == "." {
            normalized.insert(".".to_string());
            continue;
        }
        let Ok(full_path) = ctx.exec.resolve_path(path) else {
            continue;
        };
        if let Ok(relative_path) = full_path.strip_prefix(&ctx.exec.workspace_root) {
            let relative = relative_path.to_string_lossy().replace('\\', "/");
            if relative.is_empty() {
                normalized.insert(".".to_string());
            } else {
                normalized.insert(relative);
            }
        }
    }

    normalized.into_iter().collect()
}

fn run_snapshot_plan(command: &str) -> Option<BashRunSnapshotPlan> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    for segment in split_shell_segments(trimmed) {
        if let Some(target) = file_write_redirection_target(segment) {
            return Some(BashRunSnapshotPlan {
                reason: BashRunSnapshotReason::FileWriteRedirection,
                detail: summarize_shell_text(segment),
                scope: BashRunSnapshotScope::Paths(vec![target]),
            });
        }

        let Some(tokens) = shlex::split(segment) else {
            continue;
        };
        let Some((command_index, executable)) = shell_executable(&tokens) else {
            continue;
        };
        let args = &tokens[command_index + 1..];

        match executable {
            "rm" | "unlink" => {
                let paths = collect_path_hints(collect_non_option_tokens(args));
                if let Some(scope) = path_scope(paths) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::Delete,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "mv" | "rename" => {
                let path_tokens = collect_non_option_tokens(args);
                if path_tokens.len() >= 2 {
                    let mut paths = path_tokens[..path_tokens.len().saturating_sub(1)].to_vec();
                    paths.push(path_tokens.last().cloned().unwrap_or_default());
                    if let Some(scope) = path_scope(collect_path_hints(paths)) {
                        return Some(BashRunSnapshotPlan {
                            reason: BashRunSnapshotReason::MoveOrRename,
                            detail: summarize_shell_text(segment),
                            scope,
                        });
                    }
                }
            }
            "cp" | "install" | "ln" => {
                let path_tokens = collect_non_option_tokens(args);
                if let Some(destination) = path_tokens.last() {
                    if let Some(scope) = path_scope(collect_path_hints(vec![destination.clone()])) {
                        return Some(BashRunSnapshotPlan {
                            reason: BashRunSnapshotReason::Copy,
                            detail: summarize_shell_text(segment),
                            scope,
                        });
                    }
                }
            }
            "mkdir" | "rmdir" => {
                if let Some(scope) = path_scope(collect_path_hints(collect_non_option_tokens(args)))
                {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::DirectoryMutation,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "touch" => {
                if let Some(scope) = path_scope(collect_path_hints(collect_non_option_tokens(args)))
                {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::Touch,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "truncate" => {
                if let Some(scope) = path_scope(collect_path_hints(truncate_target_tokens(args))) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::Truncate,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "chmod" => {
                if let Some(scope) = path_scope(collect_path_hints(permission_target_tokens(args)))
                {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::PermissionChange,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "chown" | "chgrp" => {
                if let Some(scope) = path_scope(collect_path_hints(permission_target_tokens(args)))
                {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::OwnershipChange,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "sed" => {
                if has_in_place_flag(args) {
                    if let Some(scope) = path_scope(collect_path_hints(sed_target_tokens(args))) {
                        return Some(BashRunSnapshotPlan {
                            reason: BashRunSnapshotReason::InPlaceEdit,
                            detail: summarize_shell_text(segment),
                            scope,
                        });
                    }
                }
            }
            "perl" => {
                if has_perl_in_place_flag(args) {
                    if let Some(scope) = path_scope(collect_path_hints(perl_target_tokens(args))) {
                        return Some(BashRunSnapshotPlan {
                            reason: BashRunSnapshotReason::InPlaceEdit,
                            detail: summarize_shell_text(segment),
                            scope,
                        });
                    }
                }
            }
            "tar" => {
                if let Some(scope) = tar_run_snapshot_scope(args) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::ArchiveExtraction,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "unzip" => {
                if let Some(scope) = unzip_run_snapshot_scope(args) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::ArchiveExtraction,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "7z" | "7za" | "7zr" => {
                if let Some(scope) = seven_zip_run_snapshot_scope(args) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::ArchiveExtraction,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            "git" => {
                if let Some(scope) = git_workspace_rewrite_scope(args) {
                    return Some(BashRunSnapshotPlan {
                        reason: BashRunSnapshotReason::GitWorkspaceRewrite,
                        detail: summarize_shell_text(segment),
                        scope,
                    });
                }
            }
            _ => {}
        }
    }

    None
}

pub(crate) fn risky_shell_changed_path_hints(command: &str) -> Vec<String> {
    run_snapshot_plan(command)
        .and_then(|plan| match plan.scope {
            BashRunSnapshotScope::Paths(paths) => Some(paths),
            BashRunSnapshotScope::WorkspaceTree => None,
        })
        .unwrap_or_default()
}

fn split_shell_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut chars = command.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while let Some((index, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => {
                let segment = command[start..index].trim();
                if !segment.is_empty() {
                    segments.push(segment);
                }
                start = index + ch.len_utf8();
            }
            '|' if !in_single && !in_double => {
                let is_double_pipe = chars.peek().is_some_and(|(_, next)| *next == '|');
                if is_double_pipe {
                    let segment = command[start..index].trim();
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                    let (_, next) = chars.next().expect("peeked pipe should exist");
                    start = index + ch.len_utf8() + next.len_utf8();
                } else {
                    let segment = command[start..index].trim();
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                    start = index + ch.len_utf8();
                }
            }
            '&' if !in_single && !in_double => {
                if chars.peek().is_some_and(|(_, next)| *next == '&') {
                    let segment = command[start..index].trim();
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                    let (_, next) = chars.next().expect("peeked ampersand should exist");
                    start = index + ch.len_utf8() + next.len_utf8();
                }
            }
            _ => {}
        }
    }

    let tail = command[start..].trim();
    if !tail.is_empty() {
        segments.push(tail);
    }
    segments
}

fn file_write_redirection_target(command: &str) -> Option<String> {
    let mut chars = command.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while let Some((_, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '>' if !in_single && !in_double => {
                if chars.peek().is_some_and(|(_, next)| *next == '>') {
                    chars.next();
                }

                while chars.peek().is_some_and(|(_, next)| next.is_whitespace()) {
                    chars.next();
                }

                let mut target = String::new();
                while let Some((_, next)) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '|' | ';') {
                        break;
                    }
                    target.push(*next);
                    chars.next();
                }

                if target.is_empty() || target.starts_with('&') || target == "/dev/null" {
                    continue;
                }

                if let Some(path) = shell_path_hint(&target) {
                    return Some(path);
                }
            }
            _ => {}
        }
    }

    None
}

fn shell_executable(tokens: &[String]) -> Option<(usize, &str)> {
    tokens.iter().enumerate().find_map(|(index, token)| {
        if index == 0 && token == "env" {
            return None;
        }

        if token.contains('=') && !token.starts_with('/') && !token.starts_with("./") {
            let Some((name, value)) = token.split_once('=') else {
                return Some((index, token.as_str()));
            };
            if !name.is_empty() && value.is_empty()
                || name
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
            {
                return None;
            }
        }

        Some((index, token.as_str()))
    })
}

fn collect_non_option_tokens(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

fn truncate_target_tokens(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(arg.as_str(), "-s" | "--size" | "-r" | "--reference") {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--size=") || arg.starts_with("--reference=") || arg.starts_with('-') {
            continue;
        }
        targets.push(arg.clone());
    }
    targets
}

fn permission_target_tokens(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut skipped_subject = false;
    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        if !skipped_subject {
            skipped_subject = true;
            continue;
        }
        targets.push(arg.clone());
    }
    targets
}

fn has_in_place_flag(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-i" || arg.starts_with("-i"))
}

fn has_perl_in_place_flag(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "-i"
            || arg == "-pi"
            || arg == "-ip"
            || arg.starts_with("-i")
            || arg.starts_with("-pi")
            || arg.starts_with("-ip")
    })
}

fn sed_target_tokens(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut index = 0usize;
    let mut script_seen = false;

    while index < args.len() {
        let arg = &args[index];
        if !script_seen {
            if arg == "-e" || arg == "-f" {
                index += 2;
                continue;
            }
            if arg == "-i" || arg.starts_with("-i") || arg.starts_with('-') {
                index += 1;
                continue;
            }
            script_seen = true;
            index += 1;
            continue;
        }

        targets.push(arg.clone());
        index += 1;
    }

    targets
}

fn perl_target_tokens(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut script_seen = false;

    for arg in args {
        if !script_seen {
            if arg.starts_with('-') {
                continue;
            }
            script_seen = true;
            continue;
        }
        targets.push(arg.clone());
    }

    targets
}

fn tar_run_snapshot_scope(args: &[String]) -> Option<BashRunSnapshotScope> {
    let extraction = args.iter().any(|arg| is_tar_extract_flag(arg));
    if !extraction {
        return None;
    }

    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-C" {
            if let Some(path) = args.get(index + 1).and_then(|path| shell_path_hint(path)) {
                return Some(BashRunSnapshotScope::Paths(vec![path]));
            }
            return Some(BashRunSnapshotScope::WorkspaceTree);
        }
        if let Some(path) = arg.strip_prefix("--directory=").and_then(shell_path_hint) {
            return Some(BashRunSnapshotScope::Paths(vec![path]));
        }
        index += 1;
    }

    Some(BashRunSnapshotScope::WorkspaceTree)
}

fn is_tar_extract_flag(arg: &str) -> bool {
    let trimmed = arg.trim_start_matches('-');
    !trimmed.is_empty()
        && trimmed.chars().all(|ch| ch.is_ascii_alphabetic())
        && trimmed.contains('x')
}

fn unzip_run_snapshot_scope(args: &[String]) -> Option<BashRunSnapshotScope> {
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-d" {
            if let Some(path) = args.get(index + 1).and_then(|path| shell_path_hint(path)) {
                return Some(BashRunSnapshotScope::Paths(vec![path]));
            }
            return Some(BashRunSnapshotScope::WorkspaceTree);
        }
        index += 1;
    }
    Some(BashRunSnapshotScope::WorkspaceTree)
}

fn seven_zip_run_snapshot_scope(args: &[String]) -> Option<BashRunSnapshotScope> {
    let mode = args.first()?;
    if mode != "x" && mode != "e" {
        return None;
    }

    for arg in args.iter().skip(1) {
        if let Some(path) = arg.strip_prefix("-o").and_then(shell_path_hint) {
            return Some(BashRunSnapshotScope::Paths(vec![path]));
        }
    }

    Some(BashRunSnapshotScope::WorkspaceTree)
}

fn git_workspace_rewrite_scope(args: &[String]) -> Option<BashRunSnapshotScope> {
    let subcommand = args.first().map(String::as_str)?;

    match subcommand {
        "reset" => args
            .iter()
            .any(|arg| arg == "--hard")
            .then_some(BashRunSnapshotScope::WorkspaceTree),
        "clean" => args
            .iter()
            .any(|arg| arg.starts_with("-f") || arg == "--force")
            .then_some(BashRunSnapshotScope::WorkspaceTree),
        "checkout" => {
            if let Some(double_dash) = args.iter().position(|arg| arg == "--") {
                let paths = collect_path_hints(args[double_dash + 1..].to_vec());
                return path_scope(paths).or(Some(BashRunSnapshotScope::WorkspaceTree));
            }
            if args.len() > 1 {
                return Some(BashRunSnapshotScope::WorkspaceTree);
            }
            None
        }
        "restore" => {
            if let Some(double_dash) = args.iter().position(|arg| arg == "--") {
                let paths = collect_path_hints(args[double_dash + 1..].to_vec());
                return path_scope(paths).or(Some(BashRunSnapshotScope::WorkspaceTree));
            }

            let paths = collect_path_hints(
                args.iter()
                    .skip(1)
                    .filter(|arg| !arg.starts_with('-'))
                    .cloned()
                    .collect(),
            );
            path_scope(paths).or(Some(BashRunSnapshotScope::WorkspaceTree))
        }
        "apply" => Some(BashRunSnapshotScope::WorkspaceTree),
        _ => None,
    }
}

fn collect_path_hints(tokens: Vec<String>) -> Vec<String> {
    tokens
        .into_iter()
        .filter_map(|token| shell_path_hint(&token))
        .collect()
}

fn path_scope(paths: Vec<String>) -> Option<BashRunSnapshotScope> {
    if paths.is_empty() {
        None
    } else if paths.iter().any(|path| path == ".") {
        Some(BashRunSnapshotScope::WorkspaceTree)
    } else {
        Some(BashRunSnapshotScope::Paths(paths))
    }
}

fn shell_path_hint(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "." || trimmed == "./" {
        return Some(".".to_string());
    }
    if trimmed.starts_with('/') || trimmed.starts_with("~/") || trimmed.starts_with('$') {
        return None;
    }

    let mut end = trimmed.len();
    for (index, ch) in trimmed.char_indices() {
        if matches!(ch, '*' | '?' | '[') {
            end = index;
            break;
        }
    }
    let candidate = trimmed[..end]
        .trim_end_matches('/')
        .trim_start_matches("./")
        .trim();

    if candidate.is_empty() {
        if trimmed.starts_with("./") {
            Some(".".to_string())
        } else {
            None
        }
    } else {
        Some(candidate.to_string())
    }
}

fn summarize_shell_text(text: &str) -> String {
    const MAX_CHARS: usize = 96;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= MAX_CHARS {
        return compact;
    }

    let mut end = MAX_CHARS;
    while end > 0 && !compact.is_char_boundary(end) {
        end -= 1;
    }
    let mut limited = compact[..end].trim_end().to_string();
    limited.push_str("...");
    limited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use crate::{CancellationToken, WorkspaceRunSnapshotStore};
    use std::fs;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_bash_run_snapshot_plan_classifies_expected_cases() {
        let plan = run_snapshot_plan("echo hello > notes.txt").unwrap();
        assert_eq!(plan.reason, BashRunSnapshotReason::FileWriteRedirection);
        assert_eq!(
            plan.scope,
            BashRunSnapshotScope::Paths(vec!["notes.txt".to_string()])
        );

        let plan = run_snapshot_plan("rm -rf src").unwrap();
        assert_eq!(plan.reason, BashRunSnapshotReason::Delete);

        let plan = run_snapshot_plan("sed -i 's/a/b/' src/lib.rs").unwrap();
        assert_eq!(plan.reason, BashRunSnapshotReason::InPlaceEdit);

        let plan = run_snapshot_plan("git reset --hard").unwrap();
        assert_eq!(plan.scope, BashRunSnapshotScope::WorkspaceTree);

        assert!(run_snapshot_plan("pwd").is_none());
        assert!(run_snapshot_plan("git status").is_none());
        assert!(run_snapshot_plan("cargo test --lib").is_none());
    }

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

    #[test]
    fn test_read_only_bash_does_not_create_run_snapshot() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf())
            .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                temp.path().to_path_buf(),
            ));
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();

        tool.execute(serde_json::json!({"command": "pwd"}), &ctx)
            .unwrap();

        assert_eq!(
            exec.run_snapshot_store().unwrap().latest_status().unwrap(),
            None
        );
    }

    #[test]
    fn test_risky_bash_creates_run_snapshot_with_bash_metadata() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf())
            .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                temp.path().to_path_buf(),
            ));
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();

        tool.execute(
            serde_json::json!({"command": "echo hello > notes.txt"}),
            &ctx,
        )
        .unwrap();

        let status = exec
            .run_snapshot_store()
            .unwrap()
            .latest_status()
            .unwrap()
            .unwrap();
        assert_eq!(status.captures.len(), 1);
        assert_eq!(status.captures[0].source, RunSnapshotCaptureSource::Bash);
        assert_eq!(status.captures[0].reason, "shell redirection write");
        assert!(status.captures[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("notes.txt"));
        assert_eq!(status.captured_paths, vec!["notes.txt"]);
    }

    #[test]
    fn test_failed_risky_bash_still_leaves_run_snapshot() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf())
            .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                temp.path().to_path_buf(),
            ));
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();

        let result = tool.execute(
            serde_json::json!({"command": "echo hello > notes.txt; exit 1"}),
            &ctx,
        );

        assert!(result.unwrap().contains("Exit code: 1"));
        let status = exec.run_snapshot_store().unwrap().latest_status().unwrap();
        assert!(status.is_some());
    }

    #[test]
    fn test_restore_after_risky_bash_move_recovers_workspace() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("before.txt"), "before").unwrap();

        let exec = ExecutionContext::new(temp.path().to_path_buf())
            .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                temp.path().to_path_buf(),
            ));
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = BashTool::new();

        tool.execute(
            serde_json::json!({"command": "mv before.txt after.txt"}),
            &ctx,
        )
        .unwrap();

        exec.run_snapshot_store()
            .unwrap()
            .restore_latest()
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("before.txt")).unwrap(),
            "before"
        );
        assert!(!temp.path().join("after.txt").exists());
    }
}

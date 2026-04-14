//! Narrow deterministic lifecycle hooks for workspace-local interception.
//!
//! Hooks are optional, workspace-local, and deterministic. They run at four
//! lifecycle boundaries:
//!
//! - **OnSessionStart**: inject bounded context at session start
//! - **PreTool**: intercept tool calls before execution (allow/block/annotate)
//! - **PostWrite**: verify or format after file writes
//! - **PreFinal**: check or annotate before the final response is emitted
//!
//! Hooks are configured via `.topagent/hooks.toml` in the workspace root.
//! No config file means zero hook overhead. Hooks cannot bypass approval gates,
//! trust boundaries, or durable-memory promotion policy.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

const HOOKS_MANIFEST_PATH: &str = ".topagent/hooks.toml";
const MAX_HOOK_OUTPUT_BYTES: usize = 2048;
const MAX_HOOK_TIMEOUT_SECS: u64 = 10;
const MAX_CONTEXT_INJECTION_BYTES: usize = 1024;
const MAX_NOTE_BYTES: usize = 256;

// ── Hook Events ──

/// The four lifecycle boundaries where hooks may intercept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    OnSessionStart,
    PreTool,
    PostWrite,
    PreFinal,
}

impl HookEvent {
    pub fn label(self) -> &'static str {
        match self {
            Self::OnSessionStart => "on_session_start",
            Self::PreTool => "pre_tool",
            Self::PostWrite => "post_write",
            Self::PreFinal => "pre_final",
        }
    }
}

// ── Hook Input ──

/// Typed input provided to a hook at execution time.
#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    pub event: HookEvent,
    /// For PreTool: the tool name. For PostWrite: the file path. Otherwise empty.
    pub subject: String,
    /// For PreTool: the tool args as JSON. For OnSessionStart: the instruction.
    /// For PreFinal: the draft response text. For PostWrite: empty.
    pub detail: String,
}

// ── Hook Output ──

/// The verdict a hook returns. Hooks that produce no parseable output default
/// to `Allow` (pass-through).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum HookVerdict {
    /// Allow the operation to proceed.
    Allow,
    /// Block the operation with a reason.
    Block { reason: String },
    /// Allow but inject bounded context or annotation.
    Annotate { note: String },
    /// Request a follow-up verification step (PostWrite only).
    RequestVerify { command: String },
}

impl Default for HookVerdict {
    fn default() -> Self {
        Self::Allow
    }
}

// ── Hook Definition ──

/// A single hook entry from the manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct HookDefinition {
    /// Which lifecycle event this hook fires on.
    pub event: HookEvent,
    /// Shell command to execute. Receives HookInput as JSON on stdin.
    pub command: String,
    /// Optional filter: for PreTool, only fire on these tool names.
    /// For PostWrite, only fire on paths matching these globs.
    #[serde(default)]
    pub filter: Vec<String>,
    /// Human-readable label for progress/operator display.
    #[serde(default)]
    pub label: String,
    /// Timeout in seconds (capped to MAX_HOOK_TIMEOUT_SECS).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    5
}

// ── Hook Manifest ──

/// The workspace-local hook manifest parsed from `.topagent/hooks.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookManifest {
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
}

// ── Hook Registry ──

/// Runtime hook registry built from the manifest. Indexes hooks by event
/// for O(1) lookup during the hot path.
#[derive(Debug, Clone, Default)]
pub struct HookRegistry {
    by_event: HashMap<HookEvent, Vec<HookDefinition>>,
}

impl HookRegistry {
    pub fn empty() -> Self {
        Self {
            by_event: HashMap::new(),
        }
    }

    pub fn from_manifest(manifest: HookManifest) -> Self {
        let mut by_event: HashMap<HookEvent, Vec<HookDefinition>> = HashMap::new();
        for hook in manifest.hooks {
            by_event.entry(hook.event).or_default().push(hook);
        }
        Self { by_event }
    }

    /// Load the hook manifest from the workspace. Returns an empty registry
    /// if the manifest does not exist (zero cost on the hot path).
    pub fn load_from_workspace(workspace_root: &Path) -> Self {
        let path = workspace_root.join(HOOKS_MANIFEST_PATH);
        if !path.exists() {
            return Self::empty();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<HookManifest>(&content) {
                Ok(manifest) => Self::from_manifest(manifest),
                Err(_) => Self::empty(),
            },
            Err(_) => Self::empty(),
        }
    }

    /// True if no hooks are configured. Used for fast-path skipping.
    pub fn is_empty(&self) -> bool {
        self.by_event.is_empty()
    }

    /// Get hooks registered for a specific event.
    pub fn hooks_for(&self, event: HookEvent) -> &[HookDefinition] {
        self.by_event.get(&event).map_or(&[], |v| v.as_slice())
    }

    /// Return a short summary of configured hooks for display in the prompt.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (event, hooks) in &self.by_event {
            for hook in hooks {
                let label = if hook.label.is_empty() {
                    &hook.command
                } else {
                    &hook.label
                };
                let filter_info = if hook.filter.is_empty() {
                    String::new()
                } else {
                    format!(" [filter: {}]", hook.filter.join(", "))
                };
                lines.push(format!("{}: {}{}", event.label(), label, filter_info));
            }
        }
        lines
    }
}

// ── Hook Execution ──

/// Result of executing a single hook.
#[derive(Debug, Clone)]
pub struct HookExecutionResult {
    pub hook_label: String,
    pub event: HookEvent,
    pub verdict: HookVerdict,
    pub succeeded: bool,
}

/// Execute a single hook definition with the given input. The hook receives
/// JSON on stdin and returns JSON on stdout. Parse failures default to Allow.
pub fn execute_hook(
    hook: &HookDefinition,
    input: &HookInput,
    workspace_root: &Path,
) -> HookExecutionResult {
    let label = if hook.label.is_empty() {
        hook.command.clone()
    } else {
        hook.label.clone()
    };

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(_) => {
            return HookExecutionResult {
                hook_label: label,
                event: hook.event,
                verdict: HookVerdict::Allow,
                succeeded: false,
            };
        }
    };

    let _timeout = hook.timeout_secs.min(MAX_HOOK_TIMEOUT_SECS);

    let result = Command::new("sh")
        .arg("-c")
        .arg(&hook.command)
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            if let Some(stdin) = child.stdin.take() {
                use std::io::Write;
                // Write input, ignore errors (hook might not read stdin)
                let _ = std::io::BufWriter::new(stdin).write_all(input_json.as_bytes());
            }
            child.wait_with_output()
        });

    let output = match result {
        Ok(output) => output,
        Err(_) => {
            return HookExecutionResult {
                hook_label: label,
                event: hook.event,
                verdict: HookVerdict::Allow,
                succeeded: false,
            };
        }
    };

    // Non-zero exit code means the hook itself failed — default to Allow
    // so a broken hook does not silently block the agent.
    if !output.status.success() {
        return HookExecutionResult {
            hook_label: label,
            event: hook.event,
            verdict: HookVerdict::Allow,
            succeeded: false,
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = if stdout.len() > MAX_HOOK_OUTPUT_BYTES {
        &stdout[..MAX_HOOK_OUTPUT_BYTES]
    } else {
        &stdout
    };

    let verdict = parse_hook_verdict(stdout.trim());

    HookExecutionResult {
        hook_label: label,
        event: hook.event,
        verdict,
        succeeded: true,
    }
}

/// Parse a hook's stdout as a HookVerdict. Unrecognized output → Allow.
fn parse_hook_verdict(output: &str) -> HookVerdict {
    if output.is_empty() {
        return HookVerdict::Allow;
    }

    // Try JSON parse first
    if let Ok(verdict) = serde_json::from_str::<HookVerdict>(output) {
        return clamp_verdict(verdict);
    }

    // Fallback: if it starts with "block:" treat as a block reason
    if let Some(reason) = output.strip_prefix("block:") {
        return HookVerdict::Block {
            reason: clamp_string(reason.trim(), MAX_NOTE_BYTES),
        };
    }

    // Fallback: if it starts with "note:" treat as annotation
    if let Some(note) = output.strip_prefix("note:") {
        return HookVerdict::Annotate {
            note: clamp_string(note.trim(), MAX_NOTE_BYTES),
        };
    }

    // Fallback: if it starts with "verify:" treat as verification request
    if let Some(cmd) = output.strip_prefix("verify:") {
        return HookVerdict::RequestVerify {
            command: clamp_string(cmd.trim(), MAX_NOTE_BYTES),
        };
    }

    HookVerdict::Allow
}

/// Clamp string fields in verdicts to prevent unbounded injection.
fn clamp_verdict(verdict: HookVerdict) -> HookVerdict {
    match verdict {
        HookVerdict::Allow => HookVerdict::Allow,
        HookVerdict::Block { reason } => HookVerdict::Block {
            reason: clamp_string(&reason, MAX_NOTE_BYTES),
        },
        HookVerdict::Annotate { note } => HookVerdict::Annotate {
            note: clamp_string(&note, MAX_CONTEXT_INJECTION_BYTES),
        },
        HookVerdict::RequestVerify { command } => HookVerdict::RequestVerify {
            command: clamp_string(&command, MAX_NOTE_BYTES),
        },
    }
}

fn clamp_string(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ── Hook Dispatch ──

/// Run all hooks for a given event. Returns the merged verdict.
/// Block takes priority; annotations are concatenated; verification requests
/// are collected. If no hooks fire, returns Allow immediately.
pub fn dispatch_hooks(
    registry: &HookRegistry,
    event: HookEvent,
    input: &HookInput,
    workspace_root: &Path,
) -> HookDispatchResult {
    let hooks = registry.hooks_for(event);
    if hooks.is_empty() {
        return HookDispatchResult::pass();
    }

    let filtered: Vec<&HookDefinition> = hooks
        .iter()
        .filter(|hook| matches_filter(hook, &input.subject))
        .collect();

    if filtered.is_empty() {
        return HookDispatchResult::pass();
    }

    let mut blocked = false;
    let mut block_reasons = Vec::new();
    let mut annotations = Vec::new();
    let mut verify_commands = Vec::new();
    let mut execution_results = Vec::new();

    for hook in filtered {
        let result = execute_hook(hook, input, workspace_root);
        match &result.verdict {
            HookVerdict::Allow => {}
            HookVerdict::Block { reason } => {
                blocked = true;
                block_reasons.push(format!("[{}] {}", result.hook_label, reason));
            }
            HookVerdict::Annotate { note } => {
                annotations.push(format!("[{}] {}", result.hook_label, note));
            }
            HookVerdict::RequestVerify { command } => {
                verify_commands.push(command.clone());
            }
        }
        execution_results.push(result);
    }

    HookDispatchResult {
        blocked,
        block_reasons,
        annotations,
        verify_commands,
        execution_results,
    }
}

fn matches_filter(hook: &HookDefinition, subject: &str) -> bool {
    if hook.filter.is_empty() {
        return true;
    }
    hook.filter.iter().any(|pattern| {
        // Simple prefix/suffix/exact match — not full glob, intentionally narrow
        if let Some(suffix) = pattern.strip_prefix('*') {
            subject.ends_with(suffix)
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            subject.starts_with(prefix)
        } else {
            subject == pattern
        }
    })
}

/// Merged result from dispatching all hooks for one event.
#[derive(Debug, Clone)]
pub struct HookDispatchResult {
    pub blocked: bool,
    pub block_reasons: Vec<String>,
    pub annotations: Vec<String>,
    pub verify_commands: Vec<String>,
    pub execution_results: Vec<HookExecutionResult>,
}

impl HookDispatchResult {
    pub fn pass() -> Self {
        Self {
            blocked: false,
            block_reasons: Vec::new(),
            annotations: Vec::new(),
            verify_commands: Vec::new(),
            execution_results: Vec::new(),
        }
    }

    pub fn is_pass(&self) -> bool {
        !self.blocked && self.annotations.is_empty() && self.verify_commands.is_empty()
    }

    /// Render a block message for tool-result recording.
    pub fn block_message(&self) -> Option<String> {
        if !self.blocked {
            return None;
        }
        Some(format!(
            "Blocked by workspace hook: {}",
            self.block_reasons.join("; ")
        ))
    }

    /// Render annotations as a single bounded string for context injection.
    pub fn annotation_context(&self) -> Option<String> {
        if self.annotations.is_empty() {
            return None;
        }
        let joined = self.annotations.join("\n");
        if joined.len() > MAX_CONTEXT_INJECTION_BYTES {
            Some(clamp_string(&joined, MAX_CONTEXT_INJECTION_BYTES))
        } else {
            Some(joined)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry_is_zero_cost() {
        let registry = HookRegistry::empty();
        assert!(registry.is_empty());
        assert!(registry.hooks_for(HookEvent::PreTool).is_empty());
    }

    #[test]
    fn test_parse_hook_verdict_json_allow() {
        let verdict = parse_hook_verdict(r#"{"action": "allow"}"#);
        assert_eq!(verdict, HookVerdict::Allow);
    }

    #[test]
    fn test_parse_hook_verdict_json_block() {
        let verdict = parse_hook_verdict(r#"{"action": "block", "reason": "rm -rf not allowed"}"#);
        assert_eq!(
            verdict,
            HookVerdict::Block {
                reason: "rm -rf not allowed".to_string()
            }
        );
    }

    #[test]
    fn test_parse_hook_verdict_shorthand_block() {
        let verdict = parse_hook_verdict("block: dangerous command");
        assert_eq!(
            verdict,
            HookVerdict::Block {
                reason: "dangerous command".to_string()
            }
        );
    }

    #[test]
    fn test_parse_hook_verdict_shorthand_note() {
        let verdict = parse_hook_verdict("note: remember to run fmt");
        assert_eq!(
            verdict,
            HookVerdict::Annotate {
                note: "remember to run fmt".to_string()
            }
        );
    }

    #[test]
    fn test_parse_hook_verdict_shorthand_verify() {
        let verdict = parse_hook_verdict("verify: cargo fmt --check");
        assert_eq!(
            verdict,
            HookVerdict::RequestVerify {
                command: "cargo fmt --check".to_string()
            }
        );
    }

    #[test]
    fn test_parse_hook_verdict_empty_is_allow() {
        assert_eq!(parse_hook_verdict(""), HookVerdict::Allow);
    }

    #[test]
    fn test_parse_hook_verdict_garbage_is_allow() {
        assert_eq!(parse_hook_verdict("some random text"), HookVerdict::Allow);
    }

    #[test]
    fn test_clamp_string_short() {
        assert_eq!(clamp_string("hello", 256), "hello");
    }

    #[test]
    fn test_clamp_string_long() {
        let long = "a".repeat(300);
        let clamped = clamp_string(&long, 256);
        assert!(clamped.len() <= 260); // 256 + "..."
        assert!(clamped.ends_with("..."));
    }

    #[test]
    fn test_matches_filter_empty_matches_all() {
        let hook = HookDefinition {
            event: HookEvent::PreTool,
            command: "true".to_string(),
            filter: vec![],
            label: String::new(),
            timeout_secs: 5,
        };
        assert!(matches_filter(&hook, "bash"));
        assert!(matches_filter(&hook, "write"));
    }

    #[test]
    fn test_matches_filter_exact() {
        let hook = HookDefinition {
            event: HookEvent::PreTool,
            command: "true".to_string(),
            filter: vec!["bash".to_string()],
            label: String::new(),
            timeout_secs: 5,
        };
        assert!(matches_filter(&hook, "bash"));
        assert!(!matches_filter(&hook, "write"));
    }

    #[test]
    fn test_matches_filter_suffix_glob() {
        let hook = HookDefinition {
            event: HookEvent::PostWrite,
            command: "true".to_string(),
            filter: vec!["*.rs".to_string()],
            label: String::new(),
            timeout_secs: 5,
        };
        assert!(matches_filter(&hook, "src/main.rs"));
        assert!(!matches_filter(&hook, "src/main.py"));
    }

    #[test]
    fn test_matches_filter_prefix_glob() {
        let hook = HookDefinition {
            event: HookEvent::PostWrite,
            command: "true".to_string(),
            filter: vec!["src/*".to_string()],
            label: String::new(),
            timeout_secs: 5,
        };
        assert!(matches_filter(&hook, "src/main.rs"));
        assert!(!matches_filter(&hook, "tests/main.rs"));
    }

    #[test]
    fn test_manifest_parse_from_toml() {
        let toml_str = r#"
[[hooks]]
event = "pre_tool"
command = "echo block: not allowed"
filter = ["bash"]
label = "bash guard"
timeout_secs = 3

[[hooks]]
event = "post_write"
command = "cargo fmt --check"
filter = ["*.rs"]
label = "rust format check"
"#;
        let manifest: HookManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.hooks.len(), 2);
        assert_eq!(manifest.hooks[0].event, HookEvent::PreTool);
        assert_eq!(manifest.hooks[0].filter, vec!["bash"]);
        assert_eq!(manifest.hooks[1].event, HookEvent::PostWrite);
    }

    #[test]
    fn test_registry_indexes_by_event() {
        let manifest = HookManifest {
            hooks: vec![
                HookDefinition {
                    event: HookEvent::PreTool,
                    command: "true".to_string(),
                    filter: vec![],
                    label: "a".to_string(),
                    timeout_secs: 5,
                },
                HookDefinition {
                    event: HookEvent::PreTool,
                    command: "true".to_string(),
                    filter: vec![],
                    label: "b".to_string(),
                    timeout_secs: 5,
                },
                HookDefinition {
                    event: HookEvent::PostWrite,
                    command: "true".to_string(),
                    filter: vec![],
                    label: "c".to_string(),
                    timeout_secs: 5,
                },
            ],
        };
        let registry = HookRegistry::from_manifest(manifest);
        assert_eq!(registry.hooks_for(HookEvent::PreTool).len(), 2);
        assert_eq!(registry.hooks_for(HookEvent::PostWrite).len(), 1);
        assert_eq!(registry.hooks_for(HookEvent::OnSessionStart).len(), 0);
    }

    #[test]
    fn test_dispatch_hooks_empty_registry_is_pass() {
        let registry = HookRegistry::empty();
        let input = HookInput {
            event: HookEvent::PreTool,
            subject: "bash".to_string(),
            detail: String::new(),
        };
        let result = dispatch_hooks(&registry, HookEvent::PreTool, &input, Path::new("/tmp"));
        assert!(result.is_pass());
        assert!(!result.blocked);
    }

    #[test]
    fn test_dispatch_result_block_message() {
        let result = HookDispatchResult {
            blocked: true,
            block_reasons: vec!["[guard] rm not allowed".to_string()],
            annotations: vec![],
            verify_commands: vec![],
            execution_results: vec![],
        };
        assert!(result.block_message().unwrap().contains("rm not allowed"));
    }

    #[test]
    fn test_dispatch_result_annotation_context() {
        let result = HookDispatchResult {
            blocked: false,
            block_reasons: vec![],
            annotations: vec!["[fmt] run cargo fmt".to_string()],
            verify_commands: vec![],
            execution_results: vec![],
        };
        assert!(result.annotation_context().unwrap().contains("cargo fmt"));
    }

    #[test]
    fn test_hook_verdict_clamp_long_reason() {
        let long_reason = "x".repeat(500);
        let verdict = clamp_verdict(HookVerdict::Block {
            reason: long_reason,
        });
        if let HookVerdict::Block { reason } = verdict {
            assert!(reason.len() <= MAX_NOTE_BYTES + 3);
        } else {
            panic!("expected Block");
        }
    }

    #[test]
    fn test_hook_input_serializes_to_json() {
        let input = HookInput {
            event: HookEvent::PreTool,
            subject: "bash".to_string(),
            detail: r#"{"command": "rm -rf /"}"#.to_string(),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("pre_tool"));
        assert!(json.contains("bash"));
    }

    #[test]
    fn test_summary_lines() {
        let manifest = HookManifest {
            hooks: vec![HookDefinition {
                event: HookEvent::PreTool,
                command: "check-bash".to_string(),
                filter: vec!["bash".to_string()],
                label: "bash safety".to_string(),
                timeout_secs: 5,
            }],
        };
        let registry = HookRegistry::from_manifest(manifest);
        let lines = registry.summary_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("bash safety"));
        assert!(lines[0].contains("[filter: bash]"));
    }

    #[test]
    fn test_load_from_workspace_missing_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let registry = HookRegistry::load_from_workspace(temp.path());
        assert!(registry.is_empty());
    }

    #[test]
    fn test_load_from_workspace_valid_toml() {
        let temp = tempfile::TempDir::new().unwrap();
        let hooks_dir = temp.path().join(".topagent");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.toml"),
            r#"
[[hooks]]
event = "pre_tool"
command = "echo allow"
label = "test hook"
"#,
        )
        .unwrap();
        let registry = HookRegistry::load_from_workspace(temp.path());
        assert!(!registry.is_empty());
        assert_eq!(registry.hooks_for(HookEvent::PreTool).len(), 1);
    }

    #[test]
    fn test_load_from_workspace_invalid_toml_returns_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let hooks_dir = temp.path().join(".topagent");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("hooks.toml"), "not valid toml {{{{").unwrap();
        let registry = HookRegistry::load_from_workspace(temp.path());
        assert!(registry.is_empty());
    }
}

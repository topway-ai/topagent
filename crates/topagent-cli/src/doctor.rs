use anyhow::Result;
use std::path::Path;
use topagent_core::tool_genesis::ToolGenesis;

use crate::config::{
    resolve_runtime_model_selection, resolve_workspace_path, CliParams, TOPAGENT_MODEL_KEY,
    TOPAGENT_SERVICE_MANAGED_KEY,
};
use crate::managed_files::{is_topagent_managed_file, read_managed_env_metadata};
use crate::memory::{
    MEMORY_INDEX_RELATIVE_PATH, MEMORY_LESSONS_RELATIVE_DIR, MEMORY_OBSERVATIONS_RELATIVE_DIR,
    MEMORY_PLANS_RELATIVE_DIR, MEMORY_PROCEDURES_RELATIVE_DIR, MEMORY_TOPICS_RELATIVE_DIR,
    MEMORY_TRAJECTORIES_RELATIVE_DIR,
};
use crate::operational_paths::service_paths;

use topagent_core::{load_operator_profile, user_profile_path};

const HOOKS_MANIFEST_RELATIVE_PATH: &str = ".topagent/hooks.toml";
const EXTERNAL_TOOLS_RELATIVE_PATH: &str = ".topagent/external-tools.json";
const TOOLS_DIR_RELATIVE_PATH: &str = ".topagent/tools";

const USER_MD_SIZE_WARN: usize = 2048;
const USER_MD_SIZE_ERROR: usize = 4096;
const MEMORY_MD_SIZE_WARN: usize = 1500;
const MEMORY_MD_SIZE_ERROR: usize = 3000;
const MEMORY_MD_MAX_ENTRIES: usize = 24;
const MEMORY_MD_MAX_NOTE_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckLevel {
    Ok,
    Warning,
    Error,
}

impl CheckLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

pub(crate) struct CheckResult {
    pub(crate) name: &'static str,
    pub(crate) level: CheckLevel,
    pub(crate) detail: String,
    pub(crate) hint: Option<String>,
}

pub(crate) fn run_doctor(params: CliParams) -> Result<()> {
    let checks = run_doctor_checks(&params);
    print_report(&checks);
    let has_errors = checks.iter().any(|c| c.level == CheckLevel::Error);
    if has_errors {
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) fn run_doctor_checks(params: &CliParams) -> Vec<CheckResult> {
    let mut checks = Vec::new();

    let workspace = resolve_workspace_path(params.workspace.clone());
    match workspace {
        Ok(ws) => {
            checks.push(CheckResult {
                name: "workspace path",
                level: CheckLevel::Ok,
                detail: format!("exists: {}", ws.display()),
                hint: None,
            });
            check_workspace_layout(&ws, &mut checks);
            check_workspace_files(&ws, &mut checks);
            check_generated_tools(&ws, &mut checks);
            check_external_tools(&ws, &mut checks);
            check_hooks_manifest(&ws, &mut checks);
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "workspace path",
                level: CheckLevel::Error,
                detail: format!("unresolvable: {}", err),
                hint: Some("pass --workspace /path or run from a repo directory".to_string()),
            });
        }
    }

    check_service_config(params, &mut checks);
    checks
}

fn check_workspace_layout(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let topagent_dir = workspace.join(".topagent");
    if !topagent_dir.exists() {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Error,
            detail: ".topagent/ directory does not exist".to_string(),
            hint: Some(
                "run a task to auto-create the layout, or check the workspace path".to_string(),
            ),
        });
        return;
    }

    let required_dirs = [
        MEMORY_TOPICS_RELATIVE_DIR,
        MEMORY_LESSONS_RELATIVE_DIR,
        MEMORY_PLANS_RELATIVE_DIR,
        MEMORY_PROCEDURES_RELATIVE_DIR,
        MEMORY_TRAJECTORIES_RELATIVE_DIR,
        MEMORY_OBSERVATIONS_RELATIVE_DIR,
    ];

    let mut missing = Vec::new();
    for dir_rel in &required_dirs {
        let dir = workspace.join(dir_rel);
        if !dir.is_dir() {
            missing.push(dir_rel.to_string());
        }
    }

    if missing.is_empty() {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Ok,
            detail: "all expected subdirectories present".to_string(),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "workspace layout",
            level: CheckLevel::Warning,
            detail: format!("missing directories: {}", missing.join(", ")),
            hint: Some("run a task to auto-create missing layout directories".to_string()),
        });
    }
}

fn check_workspace_files(workspace: &Path, checks: &mut Vec<CheckResult>) {
    check_memory_md(workspace, checks);
    check_user_md(workspace, checks);
}

fn check_memory_md(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let path = workspace.join(MEMORY_INDEX_RELATIVE_PATH);
    if !path.exists() {
        checks.push(CheckResult {
            name: "MEMORY.md",
            level: CheckLevel::Warning,
            detail: "file does not exist".to_string(),
            hint: Some("will be auto-created on next task run".to_string()),
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "MEMORY.md",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    let mut level = CheckLevel::Ok;
    let mut details = Vec::new();

    if raw.len() > MEMORY_MD_SIZE_ERROR {
        level = CheckLevel::Error;
        details.push(format!(
            "size {} bytes exceeds error budget ({} bytes)",
            raw.len(),
            MEMORY_MD_SIZE_ERROR
        ));
    } else if raw.len() > MEMORY_MD_SIZE_WARN {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "size {} bytes exceeds warning budget ({} bytes)",
            raw.len(),
            MEMORY_MD_SIZE_WARN
        ));
    }

    let entries: Vec<_> = raw
        .lines()
        .filter(|line| line.trim().starts_with("- "))
        .collect();

    if entries.len() > MEMORY_MD_MAX_ENTRIES {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entries exceeds budget of {}",
            entries.len(),
            MEMORY_MD_MAX_ENTRIES
        ));
    }

    let mut long_notes = 0usize;
    let mut bad_format = 0usize;
    for line in &entries {
        if let Some(note) = extract_note_from_index_line(line) {
            if note.len() > MEMORY_MD_MAX_NOTE_CHARS {
                long_notes += 1;
            }
        } else if !line.contains("|") {
            bad_format += 1;
        }
    }

    if bad_format > 0 {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entry/entries missing pipe-delimited fields",
            bad_format
        ));
    }

    if long_notes > 0 {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "{} entry/entries have notes exceeding {} chars",
            long_notes, MEMORY_MD_MAX_NOTE_CHARS
        ));
    }

    let content_issues = lint_memory_md_content(&raw);
    if !content_issues.is_empty() {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.extend(content_issues);
    }

    let detail = if details.is_empty() {
        format!("{} entries, {} bytes", entries.len(), raw.len())
    } else {
        details.join("; ")
    };

    checks.push(CheckResult {
        name: "MEMORY.md",
        level,
        detail,
        hint: if level != CheckLevel::Ok {
            Some("run `topagent procedure prune` or consolidate to reduce index bulk".to_string())
        } else {
            None
        },
    });
}

fn check_user_md(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let path = user_profile_path(workspace);
    if !path.exists() {
        checks.push(CheckResult {
            name: "USER.md",
            level: CheckLevel::Ok,
            detail: "not present (optional)".to_string(),
            hint: None,
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "USER.md",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    let mut level = CheckLevel::Ok;
    let mut details = Vec::new();

    if raw.len() > USER_MD_SIZE_ERROR {
        level = CheckLevel::Error;
        details.push(format!(
            "size {} bytes exceeds error budget ({} bytes)",
            raw.len(),
            USER_MD_SIZE_ERROR
        ));
    } else if raw.len() > USER_MD_SIZE_WARN {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.push(format!(
            "size {} bytes exceeds warning budget ({} bytes)",
            raw.len(),
            USER_MD_SIZE_WARN
        ));
    }

    match load_operator_profile(workspace) {
        Ok(profile) => {
            details.push(format!(
                "{} preference(s) parsed",
                profile.preferences.len()
            ));
        }
        Err(err) => {
            level = CheckLevel::Error;
            details.push(format!("parse error: {}", err));
        }
    }

    let content_issues = lint_user_md_content(&raw);
    if !content_issues.is_empty() {
        if level == CheckLevel::Ok {
            level = CheckLevel::Warning;
        }
        details.extend(content_issues);
    }

    checks.push(CheckResult {
        name: "USER.md",
        level,
        detail: details.join("; "),
        hint: if level != CheckLevel::Ok {
            Some("keep USER.md small: only stable operator preferences, not repo facts or task state".to_string())
        } else {
            None
        },
    });
}

fn check_generated_tools(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let tools_dir = workspace.join(TOOLS_DIR_RELATIVE_PATH);
    if !tools_dir.exists() {
        checks.push(CheckResult {
            name: "generated tools",
            level: CheckLevel::Ok,
            detail: "no .topagent/tools/ directory".to_string(),
            hint: None,
        });
        return;
    }

    let genesis = ToolGenesis::new(workspace.to_path_buf());
    match genesis.runtime_generated_tool_inventory() {
        Ok(inventory) => {
            if inventory.warnings.is_empty() {
                checks.push(CheckResult {
                    name: "generated tools",
                    level: CheckLevel::Ok,
                    detail: format!(
                        "{} tool(s) verified, 0 warnings",
                        inventory.verified_tools.len()
                    ),
                    hint: None,
                });
            } else {
                let warning_names: Vec<_> = inventory
                    .warnings
                    .iter()
                    .take(3)
                    .map(|w| format!("{}: {}", w.name, w.message))
                    .collect();
                let mut detail = format!(
                    "{} verified, {} warning(s)",
                    inventory.verified_tools.len(),
                    inventory.warnings.len()
                );
                if !warning_names.is_empty() {
                    detail.push_str("; ");
                    detail.push_str(&warning_names.join(", "));
                }
                checks.push(CheckResult {
                    name: "generated tools",
                    level: CheckLevel::Warning,
                    detail,
                    hint: Some(
                        "repair or recreate broken tools with --tool-authoring on".to_string(),
                    ),
                });
            }
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "generated tools",
                level: CheckLevel::Error,
                detail: format!("inventory scan failed: {}", err),
                hint: None,
            });
        }
    }
}

fn check_external_tools(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let path = workspace.join(EXTERNAL_TOOLS_RELATIVE_PATH);
    if !path.exists() {
        checks.push(CheckResult {
            name: "external tools",
            level: CheckLevel::Ok,
            detail: "no .topagent/external-tools.json".to_string(),
            hint: None,
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "external tools",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    let parsed: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(values) => values,
        Err(err) => {
            checks.push(CheckResult {
                name: "external tools",
                level: CheckLevel::Error,
                detail: format!("invalid JSON: {}", err),
                hint: Some("fix or remove .topagent/external-tools.json".to_string()),
            });
            return;
        }
    };

    let mut missing_sandbox = Vec::new();
    for entry in &parsed {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)");
        if entry.get("sandbox").is_none() {
            missing_sandbox.push(name.to_string());
        }
    }

    if missing_sandbox.is_empty() {
        checks.push(CheckResult {
            name: "external tools",
            level: CheckLevel::Ok,
            detail: format!("{} tool(s), all have sandbox policy", parsed.len()),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "external tools",
            level: CheckLevel::Error,
            detail: format!(
                "{} tool(s) missing required `sandbox` field: {}",
                missing_sandbox.len(),
                missing_sandbox.join(", ")
            ),
            hint: Some(
                "each external tool must declare \"sandbox\": \"workspace\" or \"sandbox\": \"host\""
                    .to_string(),
            ),
        });
    }
}

fn check_hooks_manifest(workspace: &Path, checks: &mut Vec<CheckResult>) {
    let path = workspace.join(HOOKS_MANIFEST_RELATIVE_PATH);
    if !path.exists() {
        checks.push(CheckResult {
            name: "hooks manifest",
            level: CheckLevel::Ok,
            detail: "no .topagent/hooks.toml (optional)".to_string(),
            hint: None,
        });
        return;
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            checks.push(CheckResult {
                name: "hooks manifest",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
            return;
        }
    };

    match raw.parse::<toml::Value>() {
        Ok(_) => {
            let event_count = count_hook_events(&raw);
            checks.push(CheckResult {
                name: "hooks manifest",
                level: CheckLevel::Ok,
                detail: format!("valid TOML, {} hook(s) defined", event_count),
                hint: None,
            });
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "hooks manifest",
                level: CheckLevel::Error,
                detail: format!("invalid TOML: {}", err),
                hint: Some("fix or remove .topagent/hooks.toml".to_string()),
            });
        }
    }
}

fn count_hook_events(raw: &str) -> usize {
    let parsed: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(_) => return 0,
    };
    parsed
        .get("hooks")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0)
}

fn check_service_config(params: &CliParams, checks: &mut Vec<CheckResult>) {
    let paths = match service_paths() {
        Ok(paths) => paths,
        Err(err) => {
            checks.push(CheckResult {
                name: "service config",
                level: CheckLevel::Error,
                detail: format!("cannot resolve config paths: {}", err),
                hint: None,
            });
            return;
        }
    };

    check_api_key(params, &paths, checks);
    check_model_config(params, &paths, checks);
    check_managed_env(&paths, checks);
    check_telegram_token(&paths, checks);
    check_service_install(&paths, checks);
}

fn check_api_key(
    params: &CliParams,
    _paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let from_env = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let from_cli = params.api_key.as_deref().filter(|v| !v.trim().is_empty());

    if from_cli.is_some() || from_env.is_some() {
        let source = if from_cli.is_some() {
            "CLI flag"
        } else {
            "OPENROUTER_API_KEY env"
        };
        checks.push(CheckResult {
            name: "OpenRouter API key",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "OpenRouter API key",
            level: CheckLevel::Error,
            detail: "not found in env or CLI flag".to_string(),
            hint: Some("set OPENROUTER_API_KEY or pass --api-key".to_string()),
        });
    }

    let opencode_from_env = std::env::var("OPENCODE_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let opencode_from_cli = params
        .opencode_api_key
        .as_deref()
        .filter(|v| !v.trim().is_empty());

    if opencode_from_cli.is_some() || opencode_from_env.is_some() {
        let source = if opencode_from_cli.is_some() {
            "CLI flag"
        } else {
            "OPENCODE_API_KEY env"
        };
        checks.push(CheckResult {
            name: "Opencode API key",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    }
}

fn check_model_config(
    params: &CliParams,
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let persisted_model = env_values
        .get(TOPAGENT_MODEL_KEY)
        .filter(|v| !v.trim().is_empty())
        .map(String::from);

    let selection = resolve_runtime_model_selection(params.model.clone(), persisted_model);

    if selection.effective.source == crate::config::ModelResolutionSource::BuiltInFallback {
        checks.push(CheckResult {
            name: "model config",
            level: CheckLevel::Warning,
            detail: format!("using built-in default: {}", selection.effective.model_id),
            hint: Some(
                "run `topagent model pick` or `topagent model set <id>` to configure a model"
                    .to_string(),
            ),
        });
    } else {
        checks.push(CheckResult {
            name: "model config",
            level: CheckLevel::Ok,
            detail: format!(
                "{} ({})",
                selection.effective.model_id,
                selection.effective.source.label()
            ),
            hint: None,
        });
    }
}

fn check_managed_env(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    if !paths.env_path.exists() {
        checks.push(CheckResult {
            name: "managed env/config",
            level: CheckLevel::Warning,
            detail: "env file does not exist".to_string(),
            hint: Some("run `topagent install` to create managed config".to_string()),
        });
        return;
    }

    match read_managed_env_metadata(&paths.env_path) {
        Ok(values) => {
            let is_managed = values
                .get(TOPAGENT_SERVICE_MANAGED_KEY)
                .is_some_and(|v| v == "1");
            if is_managed {
                let key_count = values.len();
                checks.push(CheckResult {
                    name: "managed env/config",
                    level: CheckLevel::Ok,
                    detail: format!("readable, {} key(s), managed", key_count),
                    hint: None,
                });
            } else {
                checks.push(CheckResult {
                    name: "managed env/config",
                    level: CheckLevel::Warning,
                    detail: "file exists but not managed by TopAgent".to_string(),
                    hint: None,
                });
            }
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "managed env/config",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
        }
    }
}

fn check_telegram_token(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let from_env = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let from_config = values
        .get("TELEGRAM_BOT_TOKEN")
        .filter(|v| !v.trim().is_empty());

    if from_env.is_some() || from_config.is_some() {
        let source = if from_env.is_some() {
            "env"
        } else {
            "managed config"
        };
        checks.push(CheckResult {
            name: "Telegram token",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "Telegram token",
            level: CheckLevel::Warning,
            detail: "not found in env or managed config".to_string(),
            hint: Some("set TELEGRAM_BOT_TOKEN or run `topagent install`".to_string()),
        });
    }
}

fn check_service_install(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let unit_exists = paths.unit_path.exists();
    let env_exists = paths.env_path.exists();

    if !unit_exists && !env_exists {
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Warning,
            detail: "neither unit file nor env file installed".to_string(),
            hint: Some("run `topagent install` to set up the Telegram service".to_string()),
        });
        return;
    }

    let managed_unit = if unit_exists {
        is_topagent_managed_file(&paths.unit_path).unwrap_or(false)
    } else {
        false
    };
    let managed_env = if env_exists {
        is_topagent_managed_file(&paths.env_path).unwrap_or(false)
    } else {
        false
    };

    if managed_unit && managed_env {
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Ok,
            detail: "unit file and env file installed and managed".to_string(),
            hint: None,
        });
    } else {
        let mut issues = Vec::new();
        if unit_exists && !managed_unit {
            issues.push("unit file not managed by TopAgent");
        }
        if !unit_exists {
            issues.push("unit file missing");
        }
        if env_exists && !managed_env {
            issues.push("env file not managed by TopAgent");
        }
        if !env_exists {
            issues.push("env file missing");
        }
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Warning,
            detail: issues.join("; "),
            hint: Some("run `topagent install` to repair".to_string()),
        });
    }
}

pub(crate) fn lint_memory_md_content(raw: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = raw.to_ascii_lowercase();

    let transient_markers = [
        ("task completed", "transient session outcome"),
        ("task failed", "transient session outcome"),
        ("ran successfully", "transient session outcome"),
        ("just ran", "transient session outcome"),
        ("currently running", "transient session outcome"),
        ("pending approval", "task-local state"),
        ("waiting for", "task-local state"),
        ("todo:", "task-local state"),
        ("fixme:", "task-local state"),
    ];

    let mut transient_count = 0usize;
    for (marker, _label) in &transient_markers {
        if lower.contains(marker) {
            transient_count += 1;
        }
    }
    if transient_count > 0 {
        issues.push(format!(
            "{} transient/task-local marker(s) detected",
            transient_count
        ));
    }

    let transcript_markers = ["assistant:", "user:", "tool_result:", "tool_call:", "```"];
    let mut transcript_count = 0usize;
    for marker in &transcript_markers {
        if lower.contains(marker) {
            transcript_count += 1;
        }
    }
    if transcript_count > 0 {
        issues.push(format!(
            "{} raw transcript marker(s) detected",
            transcript_count
        ));
    }

    let mut procedure_like = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- ") && trimmed.contains("procedure") && trimmed.contains("step") {
            procedure_like += 1;
        }
    }
    if procedure_like > 0 {
        issues.push(format!(
            "{} procedure-like entries belong in .topagent/procedures/",
            procedure_like
        ));
    }

    let verbose_markers = [
        "the agent should",
        "the agent will",
        "the agent can",
        "remember to always",
        "important: make sure",
    ];
    let mut verbose_count = 0usize;
    for marker in &verbose_markers {
        if lower.contains(marker) {
            verbose_count += 1;
        }
    }
    if verbose_count > 0 {
        issues.push(format!(
            "{} verbose/instructional marker(s) detected",
            verbose_count
        ));
    }

    issues
}

pub(crate) fn lint_user_md_content(raw: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = raw.to_ascii_lowercase();

    let forbidden = [
        ("architecture", "repo fact — belongs in topics/"),
        ("runtime behavior", "repo fact — belongs in topics/"),
        ("api endpoint", "repo fact — belongs in topics/"),
        ("database schema", "repo fact — belongs in topics/"),
        ("file structure", "repo fact — belongs in topics/"),
        ("task completed", "transient session outcome"),
        ("just ran", "transient session outcome"),
        ("todo:", "task-local state"),
        ("fixme:", "task-local state"),
    ];

    let mut forbidden_count = 0usize;
    for (marker, _label) in &forbidden {
        if lower.contains(marker) {
            forbidden_count += 1;
        }
    }
    if forbidden_count > 0 {
        issues.push(format!(
            "{} forbidden content marker(s) detected (repo facts or session state)",
            forbidden_count
        ));
    }

    let transcript_markers = ["assistant:", "user:", "tool_result:", "```"];
    let mut transcript_count = 0usize;
    for marker in &transcript_markers {
        if lower.contains(marker) {
            transcript_count += 1;
        }
    }
    if transcript_count > 0 {
        issues.push(format!(
            "{} raw transcript marker(s) detected",
            transcript_count
        ));
    }

    issues
}

fn extract_note_from_index_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with("- ") {
        return None;
    }
    trimmed.split('|').find_map(|part| {
        let (key, value) = part.split_once(':')?;
        (key.trim().eq_ignore_ascii_case("note")).then_some(value.trim())
    })
}

fn print_report(checks: &[CheckResult]) {
    println!("TopAgent doctor");
    println!("{}", "-".repeat(40));

    let mut ok_count = 0usize;
    let mut warn_count = 0usize;
    let mut err_count = 0usize;

    for check in checks {
        match check.level {
            CheckLevel::Ok => ok_count += 1,
            CheckLevel::Warning => warn_count += 1,
            CheckLevel::Error => err_count += 1,
        }

        println!("[{}] {}: {}", check.level.label(), check.name, check.detail);
        if let Some(hint) = &check.hint {
            println!("      hint: {}", hint);
        }
    }

    println!("{}", "-".repeat(40));
    println!(
        "Summary: {} OK, {} warning(s), {} error(s)",
        ok_count, warn_count, err_count
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn healthy_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();

        std::fs::create_dir_all(ws.join(".topagent/topics")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/lessons")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/plans")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/procedures")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/trajectories")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/observations")).unwrap();

        std::fs::write(
            ws.join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        temp
    }

    #[test]
    fn test_doctor_succeeds_on_healthy_fixture() {
        let temp = healthy_workspace();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks.iter().all(|c| c.level == CheckLevel::Ok));
    }

    #[test]
    fn test_doctor_reports_missing_workspace_layout() {
        let temp = TempDir::new().unwrap();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Error && c.name == "workspace layout"));
    }

    #[test]
    fn test_doctor_reports_missing_model_config() {
        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let mut checks = Vec::new();
        check_model_config(&params, &service_paths().unwrap(), &mut checks);
        let model_check = checks.iter().find(|c| c.name == "model config").unwrap();
        assert!(model_check.level == CheckLevel::Ok || model_check.level == CheckLevel::Warning);
    }

    #[test]
    fn test_doctor_reports_broken_workspace_layout() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".topagent")).unwrap();
        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "workspace layout"));
    }

    #[test]
    fn test_doctor_reports_malformed_hooks() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(HOOKS_MANIFEST_RELATIVE_PATH),
            "this is not valid toml [[[",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_hooks_manifest(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Error && c.name == "hooks manifest"));
    }

    #[test]
    fn test_doctor_reports_malformed_external_tools() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(EXTERNAL_TOOLS_RELATIVE_PATH),
            "this is not json",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_external_tools(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Error && c.name == "external tools"));
    }

    #[test]
    fn test_doctor_reports_external_tools_missing_sandbox() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(EXTERNAL_TOOLS_RELATIVE_PATH),
            r#"[{"name":"bad_tool","description":"no sandbox","command":"echo","argv_template":["hello"]}]"#,
        )
        .unwrap();
        let mut checks = Vec::new();
        check_external_tools(temp.path(), &mut checks);
        assert!(checks.iter().any(|c| c.level == CheckLevel::Error
            && c.name == "external tools"
            && c.detail.contains("sandbox")));
    }

    #[test]
    fn test_doctor_valid_external_tools_ok() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(EXTERNAL_TOOLS_RELATIVE_PATH),
            r#"[{"name":"good_tool","description":"has sandbox","command":"echo","argv_template":["hello"],"sandbox":"workspace"}]"#,
        )
        .unwrap();
        let mut checks = Vec::new();
        check_external_tools(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Ok && c.name == "external tools"));
    }

    #[test]
    fn test_doctor_does_not_mutate_state() {
        let temp = healthy_workspace();
        let memory_path = temp.path().join(MEMORY_INDEX_RELATIVE_PATH);
        let before = std::fs::read_to_string(&memory_path).unwrap();

        let mut checks = Vec::new();
        check_workspace_layout(temp.path(), &mut checks);
        check_workspace_files(temp.path(), &mut checks);

        let after = std::fs::read_to_string(&memory_path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn test_doctor_memory_md_oversized_warns() {
        let temp = healthy_workspace();
        let big_content = format!(
            "# TopAgent Memory Index\n{}\n",
            "x".repeat(MEMORY_MD_SIZE_WARN + 100)
        );
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &big_content).unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "MEMORY.md"));
    }

    #[test]
    fn test_doctor_memory_md_too_many_entries_warns() {
        let temp = healthy_workspace();
        let mut content = String::from("# TopAgent Memory Index\n\n");
        for i in 0..=MEMORY_MD_MAX_ENTRIES {
            content.push_str(&format!(
                "- topic: thing_{i} | file: topics/thing_{i}.md | status: verified | note: ok\n"
            ));
        }
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &content).unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks.iter().any(|c| c.level == CheckLevel::Warning
            && c.name == "MEMORY.md"
            && c.detail.contains("budget")));
    }

    #[test]
    fn test_doctor_memory_md_bad_format_warns() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n\n- no pipe delimiters here just a long line\n",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        assert!(checks.iter().any(|c| c.level == CheckLevel::Warning
            && c.name == "MEMORY.md"
            && c.detail.contains("pipe")));
    }

    #[test]
    fn test_doctor_user_md_oversized_warns() {
        let temp = healthy_workspace();
        let mut content = String::from("# Operator Model\n\n");
        content.push_str(&"x".repeat(USER_MD_SIZE_WARN + 100));
        std::fs::write(user_profile_path(temp.path()), &content).unwrap();
        let mut checks = Vec::new();
        check_user_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "USER.md"));
    }

    #[test]
    fn test_doctor_user_md_parse_error_reports() {
        let temp = healthy_workspace();
        std::fs::write(
            user_profile_path(temp.path()),
            "# Operator Model\n\n## bad_entry\n**Not a valid section**\n",
        )
        .unwrap();
        let mut checks = Vec::new();
        check_user_md(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.name == "USER.md" && c.detail.contains("parse error")));
    }

    #[test]
    fn test_doctor_valid_hooks_ok() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(HOOKS_MANIFEST_RELATIVE_PATH),
            r#"[[hooks]]
event = "pre_tool"
command = "echo ok"
label = "test hook""#,
        )
        .unwrap();
        let mut checks = Vec::new();
        check_hooks_manifest(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Ok && c.name == "hooks manifest"));
    }

    #[test]
    fn test_doctor_generated_tools_with_warning() {
        let temp = healthy_workspace();
        let tools_dir = temp.path().join(TOOLS_DIR_RELATIVE_PATH).join("bad_tool");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(
            tools_dir.join("manifest.json"),
            r#"{"name":"bad_tool","description":"test","argv_template":[],"verified":true}"#,
        )
        .unwrap();

        let mut checks = Vec::new();
        check_generated_tools(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "generated tools"));
    }

    #[test]
    fn test_doctor_no_generated_tools_ok() {
        let temp = healthy_workspace();
        let mut checks = Vec::new();
        check_generated_tools(temp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.level == CheckLevel::Ok && c.name == "generated tools"));
    }

    #[test]
    fn test_lint_memory_md_flags_transient_content() {
        let content = "# TopAgent Memory Index\n\n- topic: deploy | file: topics/deploy.md | status: verified | note: task completed successfully\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transient")));
    }

    #[test]
    fn test_lint_memory_md_flags_transcript_content() {
        let content = "# TopAgent Memory Index\n\n- topic: chat | file: topics/chat.md | status: verified | note: assistant: fixed the bug\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transcript")));
    }

    #[test]
    fn test_lint_memory_md_flags_procedure_like_content() {
        let content = "# TopAgent Memory Index\n\n- topic: deploy procedure | file: procedures/deploy.md | status: verified | note: step-by-step deployment\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.iter().any(|i| i.contains("procedure-like")));
    }

    #[test]
    fn test_lint_memory_md_clean_content_passes() {
        let content = "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: service layout\n";
        let issues = lint_memory_md_content(content);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_lint_user_md_flags_repo_facts() {
        let content = "# Operator Model\n\n## arch_notes\n**Category:** workflow\n**Updated:** <t:1>\n**Preference:** The architecture uses microservices.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.iter().any(|i| i.contains("forbidden")));
    }

    #[test]
    fn test_lint_user_md_flags_transcript_content() {
        let content = "# Operator Model\n\n## notes\n**Category:** workflow\n**Updated:** <t:1>\n**Preference:** assistant: use concise answers.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.iter().any(|i| i.contains("transcript")));
    }

    #[test]
    fn test_lint_user_md_clean_content_passes() {
        let content = "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n";
        let issues = lint_user_md_content(content);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_doctor_full_healthy_workspace_has_no_errors() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(".topagent/hooks.toml"),
            r#"[[hooks]]
event = "pre_tool"
command = "echo ok"
label = "test hook""#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join(EXTERNAL_TOOLS_RELATIVE_PATH),
            r#"[{"name":"ls_tool","description":"list files","command":"ls","argv_template":["."],"sandbox":"workspace"}]"#,
        )
        .unwrap();

        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: None,
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let checks = run_doctor_checks(&params);
        let model_check = checks.iter().find(|c| c.name == "model config").unwrap();
        assert!(
            model_check.level == CheckLevel::Warning || model_check.level == CheckLevel::Ok,
            "model config should be warning or ok, got {:?}",
            model_check.level
        );
    }

    #[test]
    fn test_doctor_missing_topagent_dir_reports_error() {
        let temp = TempDir::new().unwrap();
        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: Some("openai/gpt-4o".to_string()),
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let checks = run_doctor_checks(&params);
        assert!(
            checks
                .iter()
                .any(|c| c.level == CheckLevel::Error && c.name == "workspace layout"),
            "missing .topagent/ should report error"
        );
    }

    #[test]
    fn test_doctor_partial_layout_reports_warnings() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
        std::fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
        std::fs::write(
            temp.path().join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        let mut layout_checks = Vec::new();
        check_workspace_layout(temp.path(), &mut layout_checks);
        assert!(layout_checks
            .iter()
            .any(|c| c.level == CheckLevel::Warning && c.name == "workspace layout"));
        let detail = layout_checks
            .iter()
            .find(|c| c.name == "workspace layout")
            .unwrap();
        assert!(detail.detail.contains("missing"));
    }

    #[test]
    fn test_doctor_memory_md_error_size_and_procedure_redirect() {
        let temp = healthy_workspace();
        let big = format!(
            "# TopAgent Memory Index\n\n{}\n",
            "x".repeat(MEMORY_MD_SIZE_ERROR + 100)
        );
        std::fs::write(temp.path().join(MEMORY_INDEX_RELATIVE_PATH), &big).unwrap();

        let mut checks = Vec::new();
        check_memory_md(temp.path(), &mut checks);
        let mem_check = checks.iter().find(|c| c.name == "MEMORY.md").unwrap();
        assert_eq!(mem_check.level, CheckLevel::Error);
        assert!(mem_check.detail.contains("error budget"));
    }

    #[test]
    fn test_doctor_read_only_guarantee() {
        let temp = healthy_workspace();
        let memory_path = temp.path().join(MEMORY_INDEX_RELATIVE_PATH);
        let user_path = user_profile_path(temp.path());
        let before_memory = std::fs::read_to_string(&memory_path).unwrap();
        let before_user = std::fs::read_to_string(&user_path).unwrap_or_default();
        let before_files = std::fs::read_dir(temp.path().join(".topagent"))
            .unwrap()
            .filter_map(|e| e.ok())
            .count();

        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: Some("openai/gpt-4o".to_string()),
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let _ = run_doctor_checks(&params);

        let after_memory = std::fs::read_to_string(&memory_path).unwrap();
        let after_user = std::fs::read_to_string(&user_path).unwrap_or_default();
        let after_files = std::fs::read_dir(temp.path().join(".topagent"))
            .unwrap()
            .filter_map(|e| e.ok())
            .count();

        assert_eq!(before_memory, after_memory, "doctor mutated MEMORY.md");
        assert_eq!(before_user, after_user, "doctor mutated USER.md");
        assert_eq!(before_files, after_files, "doctor added or removed files");
    }
}

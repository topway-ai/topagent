use crate::behavior::default_memory_policy;
use crate::capability::{AccessMode, CapabilityKind, CapabilityRequest, RiskLevel};
use crate::context::ToolContext;
use crate::operator_profile::{
    load_operator_profile, save_operator_profile, OperatorPreferenceRecord, PreferenceCategory,
    USER_PROFILE_RELATIVE_PATH,
};
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_KEY_LEN: usize = 48;
const MIN_KEY_LEN: usize = 3;
const MAX_VALUE_LEN: usize = 240;
const MAX_REASON_LEN: usize = 160;
const TRANSIENT_SCOPE_PHRASES: &[&str] = &[
    "this run",
    "this task",
    "this session",
    "for now",
    "right now",
    "temporarily",
    "today only",
    "until this task is done",
    "until this is done",
    "for the current task",
    "current objective",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorPreferenceAction {
    Set,
    Remove,
    List,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManageOperatorPreferenceArgs {
    pub action: OperatorPreferenceAction,
    pub key: Option<String>,
    pub category: Option<PreferenceCategory>,
    pub value: Option<String>,
    pub rationale: Option<String>,
}

pub struct ManageOperatorPreferenceTool;

impl ManageOperatorPreferenceTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ManageOperatorPreferenceTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for ManageOperatorPreferenceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "manage_operator_preference".to_string(),
            description: "Create, replace, remove, or list durable operator preferences. Only use for stable cross-run preferences such as response style, verification expectations, or repeatable workflow defaults.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set", "remove", "list"],
                        "description": "Set or replace a durable preference, remove one, or list all saved preferences"
                    },
                    "key": {
                        "type": "string",
                        "description": "Stable preference identifier, for example concise_final_answers or verify_rust_changes"
                    },
                    "category": {
                        "type": "string",
                        "enum": ["response_style", "workflow", "tooling", "verification"],
                        "description": "Durable preference category. Required for action=set."
                    },
                    "value": {
                        "type": "string",
                        "description": "Short durable preference statement. Required for action=set."
                    },
                    "rationale": {
                        "type": "string",
                        "description": "Optional note explaining why this preference matters across runs."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: ManageOperatorPreferenceArgs = serde_json::from_value(args).map_err(|e| {
            Error::InvalidInput(format!("manage_operator_preference: invalid input: {}", e))
        })?;

        match args.action {
            OperatorPreferenceAction::Set => set_preference(args, ctx),
            OperatorPreferenceAction::Remove => remove_preference(args, ctx),
            OperatorPreferenceAction::List => list_preferences(ctx),
        }
    }
}

fn set_preference(args: ManageOperatorPreferenceArgs, ctx: &ToolContext<'_>) -> Result<String> {
    ctx.authorize_capability(CapabilityRequest::new(
        CapabilityKind::MemoryWrite,
        USER_PROFILE_RELATIVE_PATH,
        AccessMode::Write,
        RiskLevel::High,
        "persist a durable operator preference",
    ))?;
    if let Some(reason) = default_memory_policy().memory_write_block_reason(
        "manage_operator_preference",
        ctx.run_trust_context(),
        false,
    ) {
        return Err(Error::ToolFailed(format!(
            "manage_operator_preference: {}",
            reason
        )));
    }

    let raw_key = args.key.as_deref().ok_or_else(|| {
        Error::InvalidInput(
            "manage_operator_preference: key is required for action=set".to_string(),
        )
    })?;
    let key = normalize_key(raw_key)?;
    let category = args.category.ok_or_else(|| {
        Error::InvalidInput(
            "manage_operator_preference: category is required for action=set".to_string(),
        )
    })?;
    let value = normalize_preference_text(
        "value",
        args.value.as_deref().ok_or_else(|| {
            Error::InvalidInput(
                "manage_operator_preference: value is required for action=set".to_string(),
            )
        })?,
        MAX_VALUE_LEN,
        ctx,
    )?;
    let rationale = args
        .rationale
        .as_deref()
        .map(|value| normalize_preference_text("rationale", value, MAX_REASON_LEN, ctx))
        .transpose()?;

    validate_preference_value(&value)?;
    if let Some(reason) = &rationale {
        validate_preference_value(reason)?;
    }

    let mut profile = load_operator_profile(ctx.workspace_root())?;
    let existed = profile.upsert(OperatorPreferenceRecord {
        key: key.clone(),
        category,
        value: value.clone(),
        rationale: rationale.clone(),
        updated_at: current_timestamp()?,
    });
    save_operator_profile(ctx.workspace_root(), &profile)?;

    let verb = if existed { "Updated" } else { "Stored" };
    let mut response = format!(
        "{} operator preference `{}` [{}] in {}",
        verb,
        key,
        category.as_str(),
        USER_PROFILE_RELATIVE_PATH
    );
    response.push_str(&format!("\nPreference: {}", value));
    if let Some(reason) = rationale {
        response.push_str(&format!("\nWhy: {}", reason));
    }
    Ok(response)
}

fn remove_preference(args: ManageOperatorPreferenceArgs, ctx: &ToolContext<'_>) -> Result<String> {
    ctx.authorize_capability(CapabilityRequest::new(
        CapabilityKind::MemoryWrite,
        USER_PROFILE_RELATIVE_PATH,
        AccessMode::Write,
        RiskLevel::High,
        "remove a durable operator preference",
    ))?;
    if let Some(reason) = default_memory_policy().memory_write_block_reason(
        "manage_operator_preference",
        ctx.run_trust_context(),
        false,
    ) {
        return Err(Error::ToolFailed(format!(
            "manage_operator_preference: {}",
            reason
        )));
    }

    let raw_key = args.key.as_deref().ok_or_else(|| {
        Error::InvalidInput(
            "manage_operator_preference: key is required for action=remove".to_string(),
        )
    })?;
    let key = normalize_key(raw_key)?;

    let mut profile = load_operator_profile(ctx.workspace_root())?;
    if !profile.remove(&key) {
        return Ok(format!(
            "No durable operator preference stored for `{}`.",
            key
        ));
    }

    save_operator_profile(ctx.workspace_root(), &profile)?;
    Ok(format!("Removed operator preference `{}`.", key))
}

fn list_preferences(ctx: &ToolContext<'_>) -> Result<String> {
    let profile = load_operator_profile(ctx.workspace_root())?;
    if profile.preferences.is_empty() {
        return Ok("No durable operator preferences stored.".to_string());
    }

    let mut response = format!(
        "Stored operator preferences ({}) in {}:",
        profile.preferences.len(),
        USER_PROFILE_RELATIVE_PATH
    );
    for record in profile.preferences {
        response.push_str(&format!(
            "\n- {} [{}] {}",
            record.key,
            record.category.as_str(),
            record.value
        ));
        if let Some(reason) = record.rationale {
            response.push_str(&format!(" | why: {}", reason));
        }
    }
    Ok(response)
}

fn normalize_key(raw: &str) -> Result<String> {
    let mut normalized = String::new();
    let mut just_wrote_separator = false;

    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            just_wrote_separator = false;
            continue;
        }

        if !just_wrote_separator && !normalized.is_empty() {
            normalized.push('_');
            just_wrote_separator = true;
        }
    }

    let normalized = normalized.trim_matches('_').to_string();
    if normalized.len() < MIN_KEY_LEN || normalized.len() > MAX_KEY_LEN {
        return Err(Error::InvalidInput(format!(
            "manage_operator_preference: key must normalize to {}-{} characters",
            MIN_KEY_LEN, MAX_KEY_LEN
        )));
    }
    Ok(normalized)
}

fn normalize_preference_text(
    field: &str,
    value: &str,
    max_len: usize,
    ctx: &ToolContext<'_>,
) -> Result<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err(Error::InvalidInput(format!(
            "manage_operator_preference: {} cannot be empty",
            field
        )));
    }
    if collapsed.len() > max_len {
        return Err(Error::InvalidInput(format!(
            "manage_operator_preference: {} must be at most {} characters",
            field, max_len
        )));
    }

    let redacted = ctx.secrets().redact(&collapsed);
    if redacted.as_ref() != collapsed {
        return Err(Error::InvalidInput(format!(
            "manage_operator_preference: {} contains secret-like material and cannot be stored durably",
            field
        )));
    }

    Ok(collapsed)
}

fn validate_preference_value(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if TRANSIENT_SCOPE_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return Err(Error::InvalidInput(
            "manage_operator_preference: durable preferences must be stable across runs, not tied to this task or session".to_string(),
        ));
    }
    Ok(())
}

fn current_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::ToolFailed(format!("manage_operator_preference: time error: {}", e)))
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{InfluenceMode, RunTrustContext, SourceKind, SourceLabel};
    use crate::tools::Tool;
    use tempfile::TempDir;

    fn create_tool_context() -> (ToolContext<'static>, TempDir) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let exec = Box::leak(Box::new(crate::context::ExecutionContext::new(root)));
        let runtime = Box::leak(Box::new(crate::runtime::RuntimeOptions::default()));
        (ToolContext::new(exec, runtime), temp)
    }

    #[test]
    fn test_manage_operator_preference_set_list_and_remove() {
        let (ctx, temp) = create_tool_context();
        let tool = ManageOperatorPreferenceTool::new();

        let set_result = tool.execute(
            serde_json::json!({
                "action": "set",
                "key": "concise final answers",
                "category": "response_style",
                "value": "Keep final responses concise and lead with changed files plus verification.",
                "rationale": "The operator reviews many coding runs quickly."
            }),
            &ctx,
        );
        assert!(set_result.is_ok(), "{set_result:?}");
        let set_output = set_result.unwrap();
        assert!(set_output.contains("Stored operator preference `concise_final_answers`"));

        let profile_file = temp.path().join(USER_PROFILE_RELATIVE_PATH);
        assert!(profile_file.is_file());
        let profile = std::fs::read_to_string(&profile_file).unwrap();
        assert!(profile.contains("## concise_final_answers"));
        assert!(!temp.path().join(".topagent/MEMORY.md").exists());

        let list_output = tool
            .execute(serde_json::json!({ "action": "list" }), &ctx)
            .unwrap();
        assert!(list_output.contains("Stored operator preferences (1)"));
        assert!(list_output.contains("concise_final_answers [response_style]"));

        let remove_output = tool
            .execute(
                serde_json::json!({
                    "action": "remove",
                    "key": "concise_final_answers"
                }),
                &ctx,
            )
            .unwrap();
        assert!(remove_output.contains("Removed operator preference `concise_final_answers`."));

        let profile_after = std::fs::read_to_string(&profile_file).unwrap();
        assert!(profile_after.contains("_No durable operator preferences stored yet._"));
    }

    #[test]
    fn test_manage_operator_preference_set_replaces_existing_entry_without_duplicates() {
        let (ctx, temp) = create_tool_context();
        let tool = ManageOperatorPreferenceTool::new();

        tool.execute(
            serde_json::json!({
                "action": "set",
                "key": "verify rust changes",
                "category": "verification",
                "value": "Run cargo test for rust changes."
            }),
            &ctx,
        )
        .unwrap();

        let update_output = tool
            .execute(
                serde_json::json!({
                    "action": "set",
                    "key": "verify rust changes",
                    "category": "verification",
                    "value": "Run cargo fmt and cargo test for Rust changes."
                }),
                &ctx,
            )
            .unwrap();

        assert!(update_output.contains("Updated operator preference `verify_rust_changes`"));
        let profile =
            std::fs::read_to_string(temp.path().join(USER_PROFILE_RELATIVE_PATH)).unwrap();
        assert_eq!(profile.matches("## verify_rust_changes").count(), 1);
        assert!(profile.contains("Run cargo fmt and cargo test for Rust changes."));
    }

    #[test]
    fn test_manage_operator_preference_rejects_transient_session_state() {
        let (ctx, _temp) = create_tool_context();
        let tool = ManageOperatorPreferenceTool::new();

        let result = tool.execute(
            serde_json::json!({
                "action": "set",
                "key": "temporary preference",
                "category": "workflow",
                "value": "For this task, skip running tests."
            }),
            &ctx,
        );

        assert!(matches!(result, Err(Error::InvalidInput(_))));
        let err = result.unwrap_err().to_string();
        assert!(err.contains("stable across runs"));
    }

    #[test]
    fn test_manage_operator_preference_list_empty() {
        let (ctx, _temp) = create_tool_context();
        let tool = ManageOperatorPreferenceTool::new();

        let result = tool
            .execute(serde_json::json!({ "action": "list" }), &ctx)
            .unwrap();
        assert_eq!(result, "No durable operator preferences stored.");
    }

    #[test]
    fn test_manage_operator_preference_blocks_low_trust_writes() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "2 prior transcript snippet(s)",
        ));
        let exec = Box::leak(Box::new(
            crate::context::ExecutionContext::new(root).with_run_trust_context(trust),
        ));
        let runtime = Box::leak(Box::new(crate::runtime::RuntimeOptions::default()));
        let ctx = ToolContext::new(exec, runtime);
        let tool = ManageOperatorPreferenceTool::new();

        let result = tool.execute(
            serde_json::json!({
                "action": "set",
                "key": "concise final answers",
                "category": "response_style",
                "value": "Keep final responses concise."
            }),
            &ctx,
        );

        assert!(matches!(result, Err(Error::ToolFailed(_))));
        let err = result.unwrap_err().to_string();
        assert!(err.contains("low-trust content"));
        assert!(err.contains("prior transcript"));
    }
}

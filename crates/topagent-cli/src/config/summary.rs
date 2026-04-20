use std::path::PathBuf;
use topagent_core::ProviderKind;

use crate::config::defaults::{CliParams, TelegramModeDefaults};
use crate::config::keys::{resolve_opencode_api_key, resolve_openrouter_api_key};
use crate::config::model_selection::{
    build_route_from_resolved, provider_or_default, resolve_runtime_model_selection,
};
use crate::config::runtime::{build_runtime_options, resolve_generated_tool_authoring};
use crate::config::workspace::resolve_workspace_path;

/// Secret-free summary of the resolved runtime contract, suitable for
/// operator-facing display. API key and token values are never stored here —
/// only present/missing booleans. Constructed by `resolve_contract_summary`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedContractSummary {
    pub provider: String,
    pub effective_model: String,
    /// Label describing how the effective model was resolved (e.g. "built-in default").
    pub effective_model_source_label: String,
    /// Set when the effective model differs from the configured default (CLI override active).
    pub configured_default_model: Option<String>,
    pub workspace: std::result::Result<PathBuf, String>,
    pub openrouter_key_present: bool,
    pub opencode_key_present: bool,
    pub token_present: bool,
    pub allowed_dm_username: Option<String>,
    pub bound_dm_user_id: Option<i64>,
    pub tool_authoring: bool,
    pub max_steps: usize,
    pub max_retries: usize,
    pub timeout_secs: u64,
}

/// Resolve the runtime contract into a safe, secret-free summary for display.
/// Never fails — workspace errors are captured in the `workspace` field so the
/// operator sees what is wrong rather than getting an opaque exit.
pub(crate) fn resolve_contract_summary(
    params: &CliParams,
    defaults: &TelegramModeDefaults,
) -> ResolvedContractSummary {
    let model_selection = resolve_runtime_model_selection(
        provider_or_default(defaults.provider),
        params.model.clone(),
        defaults.model.clone(),
    );
    let route = build_route_from_resolved(&model_selection.effective);

    let configured_default_model =
        if model_selection.configured_default.model_id != model_selection.effective.model_id {
            Some(model_selection.configured_default.model_id)
        } else {
            None
        };

    let workspace = resolve_workspace_path(
        params
            .workspace
            .clone()
            .or_else(|| defaults.workspace.clone()),
    )
    .map_err(|e| e.to_string());

    let options = build_runtime_options(params.max_steps, params.max_retries, params.timeout_secs)
        .with_generated_tool_authoring(resolve_generated_tool_authoring(
            params.generated_tool_authoring,
            defaults.generated_tool_authoring,
        ));

    ResolvedContractSummary {
        provider: match route.provider {
            ProviderKind::OpenRouter => "OpenRouter".to_string(),
            ProviderKind::Opencode => "Opencode".to_string(),
        },
        effective_model: model_selection.effective.model_id,
        effective_model_source_label: model_selection.effective.source.label().to_string(),
        configured_default_model,
        workspace,
        openrouter_key_present: resolve_openrouter_api_key(
            params.api_key.clone(),
            defaults.api_key.as_deref(),
        )
        .is_some(),
        opencode_key_present: resolve_opencode_api_key(
            params.opencode_api_key.clone(),
            defaults.opencode_api_key.as_deref(),
        )
        .is_some(),
        token_present: defaults.token.is_some(),
        allowed_dm_username: defaults.allowed_dm_username.clone(),
        bound_dm_user_id: defaults.bound_dm_user_id,
        tool_authoring: options.enable_generated_tool_authoring,
        max_steps: options.max_steps,
        max_retries: options.max_provider_retries,
        timeout_secs: options.provider_timeout_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::{
        TELEGRAM_ALLOWED_DM_USERNAME_KEY, TELEGRAM_BOUND_DM_USER_ID_KEY, TOPAGENT_MODEL_KEY,
        TOPAGENT_WORKSPACE_KEY, TelegramModeDefaults,
    };
    use crate::config::model_selection::SelectedProvider;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_contract_summary_shows_key_presence_without_values() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("sk-real-secret".to_string()),
            token: Some("123:token-secret".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            allowed_dm_username: Some("operator".to_string()),
            bound_dm_user_id: Some(424242),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);

        assert!(summary.openrouter_key_present);
        assert!(!summary.opencode_key_present);
        assert!(summary.token_present);
        assert_eq!(summary.allowed_dm_username.as_deref(), Some("operator"));
        assert_eq!(summary.bound_dm_user_id, Some(424242));
        assert!(summary.configured_default_model.is_none());
    }

    #[test]
    fn test_resolve_contract_summary_separates_override_from_configured_default() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("k".to_string()),
            model: Some("persisted/model".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: Some("override/model".to_string()),
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);
        assert_eq!(summary.effective_model, "override/model");
        assert_eq!(
            summary.configured_default_model.as_deref(),
            Some("persisted/model")
        );
        assert!(
            summary
                .effective_model_source_label
                .contains("CLI override")
        );
    }

    #[test]
    fn test_contract_summary_reflects_explicit_provider_not_model_heuristic() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            opencode_api_key: Some("oc-key".to_string()),
            model: Some("qwen/qwen3.6-plus".to_string()),
            provider: Some(SelectedProvider::Opencode),
            workspace: Some(workspace.path().to_path_buf()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);
        assert_eq!(summary.provider, "Opencode");
    }

    #[test]
    fn test_resolve_contract_summary_dm_access_shows_admission_state() {
        let workspace = TempDir::new().unwrap();

        let values_unbound = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "alice".to_string(),
            ),
        ]);
        let defaults_unbound = TelegramModeDefaults::from_metadata(&values_unbound);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary_unbound = resolve_contract_summary(&params, &defaults_unbound);
        assert_eq!(
            summary_unbound.allowed_dm_username.as_deref(),
            Some("alice")
        );
        assert!(summary_unbound.bound_dm_user_id.is_none());

        let mut values_bound = values_unbound.clone();
        values_bound.insert(
            TELEGRAM_BOUND_DM_USER_ID_KEY.to_string(),
            "424242".to_string(),
        );
        let defaults_bound = TelegramModeDefaults::from_metadata(&values_bound);
        let summary_bound = resolve_contract_summary(&params, &defaults_bound);
        assert_eq!(summary_bound.bound_dm_user_id, Some(424242));
        assert_eq!(summary_bound.allowed_dm_username.as_deref(), Some("alice"));
    }

    #[test]
    fn test_resolve_contract_summary_override_and_default_when_different() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                TOPAGENT_MODEL_KEY.to_string(),
                "persisted/model".to_string(),
            ),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: Some("override/model".to_string()),
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let summary = resolve_contract_summary(&params, &defaults);

        assert_eq!(summary.effective_model, "override/model");
        assert_eq!(
            summary.configured_default_model.as_deref(),
            Some("persisted/model")
        );
        assert!(
            summary
                .effective_model_source_label
                .contains("CLI override")
        );
    }
}

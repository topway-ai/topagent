use anyhow::Result;
use std::path::PathBuf;
use topagent_core::{model::ModelRoute, ProviderKind, RuntimeOptions};

use crate::config::defaults::{CliParams, TelegramModeDefaults};
use crate::config::keys::{
    require_telegram_token_with_default, resolve_opencode_api_key, resolve_openrouter_api_key,
};
use crate::config::model_selection::{
    build_route_from_resolved, provider_or_default, resolve_runtime_model_selection,
    SelectedProvider,
};
use crate::config::workspace::resolve_workspace_path;

pub(crate) fn build_runtime_options(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> RuntimeOptions {
    RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(10))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120))
}

pub(crate) fn build_runtime_options_with_defaults(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
    defaults: &TelegramModeDefaults,
) -> RuntimeOptions {
    build_runtime_options(
        max_steps.or(defaults.max_steps),
        max_retries.or(defaults.max_retries),
        timeout_secs.or(defaults.timeout_secs),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct TelegramModeConfig {
    pub token: String,
    pub openrouter_api_key: Option<String>,
    pub opencode_api_key: Option<String>,
    pub route: ModelRoute,
    /// The configured-default model (persisted or built-in fallback).
    /// May differ from `route.model_id` when a CLI `--model` override is active.
    pub configured_default_model: String,
    pub workspace: PathBuf,
    pub options: RuntimeOptions,
    pub selected_provider: SelectedProvider,
    pub allowed_dm_username: Option<String>,
    pub bound_dm_user_id: Option<i64>,
}

impl TelegramModeConfig {
    pub(crate) fn effective_api_key(&self) -> Result<String> {
        match self.route.provider {
            ProviderKind::OpenRouter => self.openrouter_api_key.clone().ok_or_else(|| {
                anyhow::anyhow!("OpenRouter API key required for OpenRouter models")
            }),
            ProviderKind::Opencode => self
                .opencode_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Opencode API key required for Opencode models")),
        }
    }
}

/// Resolved and validated runtime contract for one-shot CLI runs.
///
/// Workspace defaults to the current working directory (not the persisted
/// service workspace) so `topagent "task"` always runs against the directory
/// the user is in. The api_key is a concrete `String` (not `Option`) because
/// one-shot fails immediately when the required key is absent — there is no
/// deferred admission path like Telegram has.
#[derive(Debug, Clone)]
pub(crate) struct OneShotConfig {
    pub workspace: PathBuf,
    pub route: ModelRoute,
    /// The single API key required by `route.provider`, already validated present.
    pub api_key: String,
    pub options: RuntimeOptions,
    /// The configured-default model (persisted or built-in fallback).
    /// May differ from `route.model_id` when a `--model` override is active.
    pub configured_default_model: String,
}

pub(crate) fn resolve_telegram_mode_config(
    token: Option<String>,
    params: CliParams,
    defaults: TelegramModeDefaults,
) -> Result<TelegramModeConfig> {
    // Token is validated first so token errors are always reported before
    // API key errors (preserving the UX ordering the smoke tests rely on).
    let token = require_telegram_token_with_default(token, defaults.token.clone())?;
    let model_selection = resolve_runtime_model_selection(
        provider_or_default(defaults.provider),
        params.model,
        defaults.model.clone(),
    );
    let route = build_route_from_resolved(&model_selection.effective);
    let openrouter_api_key =
        resolve_openrouter_api_key(params.api_key, defaults.api_key.as_deref());
    let opencode_api_key = resolve_opencode_api_key(
        params.opencode_api_key,
        defaults.opencode_api_key.as_deref(),
    );

    // Fail fast: the resolved route must have its provider API key present.
    match route.provider {
        ProviderKind::OpenRouter => {
            if openrouter_api_key.is_none() {
                return Err(anyhow::anyhow!(
                    "OpenRouter API key required for model '{}': set --api-key or OPENROUTER_API_KEY",
                    route.model_id
                ));
            }
        }
        ProviderKind::Opencode => {
            if opencode_api_key.is_none() {
                return Err(anyhow::anyhow!(
                    "Opencode API key required for model '{}': set --opencode-api-key or OPENCODE_API_KEY",
                    route.model_id
                ));
            }
        }
    }

    let selected_provider = SelectedProvider::from_provider_kind(route.provider);
    Ok(TelegramModeConfig {
        token,
        openrouter_api_key,
        opencode_api_key,
        configured_default_model: model_selection.configured_default.model_id,
        route,
        workspace: resolve_workspace_path(params.workspace.or_else(|| defaults.workspace.clone()))?,
        options: build_runtime_options_with_defaults(
            params.max_steps,
            params.max_retries,
            params.timeout_secs,
            &defaults,
        ),
        selected_provider,
        allowed_dm_username: defaults.allowed_dm_username.clone(),
        bound_dm_user_id: defaults.bound_dm_user_id,
    })
}

/// Build the validated one-shot runtime contract from raw CLI params and
/// persisted defaults. Fails fast with an operator-usable error if the
/// workspace is missing or the required API key is absent for the resolved
/// provider. Token and admission fields are not part of one-shot; use
/// `resolve_telegram_mode_config` for Telegram runs.
pub(crate) fn resolve_one_shot_config(
    params: CliParams,
    defaults: TelegramModeDefaults,
) -> Result<OneShotConfig> {
    let workspace = resolve_workspace_path(params.workspace)?;
    let model_selection = resolve_runtime_model_selection(
        provider_or_default(defaults.provider),
        params.model,
        defaults.model.clone(),
    );
    let route = build_route_from_resolved(&model_selection.effective);

    // Fail fast: require the provider API key at config construction.
    let api_key = match route.provider {
        ProviderKind::OpenRouter => {
            resolve_openrouter_api_key(params.api_key, defaults.api_key.as_deref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenRouter API key required for model '{}': set --api-key or OPENROUTER_API_KEY",
                    route.model_id
                )
            })?
        }
        ProviderKind::Opencode => {
            resolve_opencode_api_key(params.opencode_api_key, defaults.opencode_api_key.as_deref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Opencode API key required for model '{}': set --opencode-api-key or OPENCODE_API_KEY",
                        route.model_id
                    )
                })?
        }
    };

    let options = build_runtime_options(params.max_steps, params.max_retries, params.timeout_secs);

    Ok(OneShotConfig {
        workspace,
        route,
        api_key,
        configured_default_model: model_selection.configured_default.model_id,
        options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::{
        TelegramModeDefaults, TOPAGENT_MAX_RETRIES_KEY, TOPAGENT_MAX_STEPS_KEY, TOPAGENT_MODEL_KEY,
        TOPAGENT_TIMEOUT_SECS_KEY, TOPAGENT_WORKSPACE_KEY,
    };
    use crate::config::model_selection::SelectedProvider;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use topagent_core::model::DEFAULT_OPENROUTER_MODEL_ID;

    #[test]
    fn test_resolve_telegram_mode_config_uses_persisted_secrets_and_model_by_default() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("persisted-key".to_string()),
            token: Some("123456:abcdef".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("persisted/model".to_string()),
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
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();

        assert_eq!(config.openrouter_api_key.as_deref(), Some("persisted-key"));
        assert_eq!(config.token, "123456:abcdef");
        assert_eq!(config.route.model_id, "persisted/model");
    }

    #[test]
    fn test_resolve_telegram_mode_config_cli_model_override_beats_persisted_default() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("persisted-key".to_string()),
            token: Some("123456:abcdef".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("persisted/model".to_string()),
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
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();

        assert_eq!(config.route.model_id, "override/model");
    }

    #[test]
    fn test_resolve_telegram_mode_config_reuses_service_managed_defaults_for_foreground_telegram() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            (
                "OPENROUTER_API_KEY".to_string(),
                "persisted-key".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "123456:abcdef".to_string(),
            ),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                TOPAGENT_MODEL_KEY.to_string(),
                "persisted/model".to_string(),
            ),
            (TOPAGENT_MAX_STEPS_KEY.to_string(), "61".to_string()),
            (TOPAGENT_MAX_RETRIES_KEY.to_string(), "4".to_string()),
            (TOPAGENT_TIMEOUT_SECS_KEY.to_string(), "75".to_string()),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();

        assert_eq!(config.openrouter_api_key.as_deref(), Some("persisted-key"));
        assert_eq!(config.token, "123456:abcdef");
        assert_eq!(config.route.model_id, "persisted/model");
        assert_eq!(config.workspace, workspace.path().canonicalize().unwrap());
        assert_eq!(config.options.max_steps, 61);
        assert_eq!(config.options.max_provider_retries, 4);
        assert_eq!(config.options.provider_timeout_secs, 75);
    }

    #[test]
    fn test_resolve_telegram_mode_config_fails_fast_when_openrouter_route_has_no_openrouter_key() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            opencode_api_key: Some("opencode-key".to_string()),
            token: Some("123:tok".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("anthropic/claude-sonnet-4.6".to_string()),
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
        };
        let err = resolve_telegram_mode_config(None, params, defaults)
            .unwrap_err()
            .to_string();
        assert!(err.contains("OpenRouter API key required"), "{err}");
        assert!(err.contains("anthropic/claude-sonnet-4.6"), "{err}");
    }

    #[test]
    fn test_resolve_telegram_mode_config_fails_fast_when_opencode_route_has_no_opencode_key() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("openrouter-key".to_string()),
            token: Some("123:tok".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("kimi-k2".to_string()),
            provider: Some(SelectedProvider::Opencode),
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
        };
        let err = resolve_telegram_mode_config(None, params, defaults)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Opencode API key required"), "{err}");
        assert!(err.contains("kimi-k2"), "{err}");
    }

    #[test]
    fn test_resolve_telegram_mode_config_populates_configured_default_model() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("k".to_string()),
            token: Some("1:t".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("persisted/model".to_string()),
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
        };
        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();
        assert_eq!(config.route.model_id, "override/model");
        assert_eq!(config.configured_default_model, "persisted/model");
    }

    #[test]
    fn test_resolve_telegram_mode_config_configured_default_falls_back_to_built_in() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("k".to_string()),
            token: Some("1:t".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: None,
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
        };
        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();
        assert_eq!(config.configured_default_model, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(config.route.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }

    #[test]
    fn test_resolve_one_shot_config_resolves_workspace_and_api_key() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("openrouter-key".to_string()),
            model: Some("persisted/model".to_string()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.workspace, workspace.path().canonicalize().unwrap());
        assert_eq!(config.api_key, "openrouter-key");
        assert_eq!(config.route.model_id, "persisted/model");
        assert_eq!(config.configured_default_model, "persisted/model");
    }

    #[test]
    fn test_resolve_one_shot_config_cli_api_key_beats_persisted() {
        let workspace = TempDir::new().unwrap();
        std::env::remove_var("OPENROUTER_API_KEY");
        let defaults = TelegramModeDefaults {
            api_key: Some("persisted-key".to_string()),
            model: Some("some/model".to_string()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: Some("cli-key".to_string()),
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.api_key, "cli-key");
    }

    #[test]
    fn test_resolve_one_shot_config_cli_opencode_key_beats_persisted() {
        let workspace = TempDir::new().unwrap();
        std::env::remove_var("OPENCODE_API_KEY");
        let defaults = TelegramModeDefaults {
            opencode_api_key: Some("persisted-key".to_string()),
            model: Some("glm-4".to_string()),
            provider: Some(SelectedProvider::Opencode),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: Some("cli-key".to_string()),
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.api_key, "cli-key");
    }

    #[test]
    fn test_resolve_one_shot_config_fails_fast_when_openrouter_key_missing() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            model: Some("some/openrouter-model".to_string()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let err = resolve_one_shot_config(params, defaults)
            .unwrap_err()
            .to_string();
        assert!(err.contains("OpenRouter API key required"), "{err}");
        assert!(err.contains("some/openrouter-model"), "{err}");
    }

    #[test]
    fn test_resolve_one_shot_config_fails_fast_when_opencode_key_missing() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            model: Some("glm-4".to_string()),
            provider: Some(SelectedProvider::Opencode),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let err = resolve_one_shot_config(params, defaults)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Opencode API key required"), "{err}");
        assert!(err.contains("glm-4"), "{err}");
    }

    #[test]
    fn test_resolve_one_shot_config_cli_override_beats_persisted_model() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("k".to_string()),
            model: Some("persisted/model".to_string()),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: Some("override/model".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.route.model_id, "override/model");
        assert_eq!(config.configured_default_model, "persisted/model");
    }

    #[test]
    fn test_resolve_one_shot_config_uses_built_in_defaults_for_steps_retries_timeout() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("k".to_string()),
            max_steps: Some(99),
            max_retries: Some(8),
            timeout_secs: Some(200),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.options.max_steps, 50);
        assert_eq!(config.options.max_provider_retries, 10);
        assert_eq!(config.options.provider_timeout_secs, 120);
    }

    #[test]
    fn test_one_shot_config_explicit_opencode_provider_routes_correctly() {
        let workspace = TempDir::new().unwrap();
        std::env::remove_var("OPENCODE_API_KEY");
        let defaults = TelegramModeDefaults {
            opencode_api_key: Some("oc-key".to_string()),
            model: Some("qwen/qwen3.6-plus".to_string()),
            provider: Some(SelectedProvider::Opencode),
            ..Default::default()
        };
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: Some(workspace.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config = resolve_one_shot_config(params, defaults).unwrap();
        assert_eq!(config.route.provider, ProviderKind::Opencode);
        assert_eq!(config.route.model_id, "qwen/qwen3.6-plus");
        assert_eq!(config.api_key, "oc-key");
    }

    #[test]
    fn test_explicit_opencode_provider_requires_opencode_key_not_openrouter() {
        let workspace = TempDir::new().unwrap();
        let defaults = TelegramModeDefaults {
            api_key: Some("openrouter-key".to_string()),
            token: Some("123:tok".to_string()),
            workspace: Some(workspace.path().to_path_buf()),
            model: Some("qwen/qwen3.6-plus".to_string()),
            provider: Some(SelectedProvider::Opencode),
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
        };
        let err = resolve_telegram_mode_config(None, params, defaults)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Opencode API key required"), "{err}");
        assert!(err.contains("qwen/qwen3.6-plus"), "{err}");
    }

    #[test]
    fn test_foreground_and_background_telegram_resolve_identical_config_from_same_metadata() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "shared-key".to_string()),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "123456:shared-token".to_string(),
            ),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (TOPAGENT_MODEL_KEY.to_string(), "shared/model".to_string()),
            (TOPAGENT_MAX_STEPS_KEY.to_string(), "65".to_string()),
            (TOPAGENT_MAX_RETRIES_KEY.to_string(), "5".to_string()),
            (TOPAGENT_TIMEOUT_SECS_KEY.to_string(), "90".to_string()),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };

        let config = resolve_telegram_mode_config(None, params.clone(), defaults.clone()).unwrap();
        assert_eq!(config.openrouter_api_key.as_deref(), Some("shared-key"));
        assert_eq!(config.token, "123456:shared-token");
        assert_eq!(config.route.model_id, "shared/model");
        assert_eq!(config.workspace, workspace.path().canonicalize().unwrap());
        assert_eq!(config.options.max_steps, 65);
        assert_eq!(config.options.max_provider_retries, 5);
        assert_eq!(config.options.provider_timeout_secs, 90);

        use crate::config::model_selection::resolve_runtime_model_selection;
        let selection = resolve_runtime_model_selection(
            provider_or_default(defaults.provider),
            None,
            defaults.model.clone(),
        );
        assert_eq!(selection.configured_default.model_id, "shared/model");
        assert_eq!(selection.effective.model_id, "shared/model");
    }

    #[test]
    fn test_cli_model_override_does_not_alter_persisted_defaults() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            (
                "OPENROUTER_API_KEY".to_string(),
                "persisted-key".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "999:persisted-token".to_string(),
            ),
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
            model: Some("cli-override/model".to_string()),
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };

        let config = resolve_telegram_mode_config(None, params, defaults.clone()).unwrap();
        assert_eq!(config.route.model_id, "cli-override/model");
        assert_eq!(defaults.model.as_deref(), Some("persisted/model"));
        assert_eq!(defaults.api_key.as_deref(), Some("persisted-key"));
        assert_eq!(defaults.token.as_deref(), Some("999:persisted-token"));
    }

    #[test]
    fn test_metadata_roundtrip_preserves_all_runtime_options() {
        let workspace = TempDir::new().unwrap();
        let original_values = HashMap::from([
            (
                "OPENROUTER_API_KEY".to_string(),
                "key-roundtrip".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "111:token-roundtrip".to_string(),
            ),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                TOPAGENT_MODEL_KEY.to_string(),
                "model/roundtrip".to_string(),
            ),
            (TOPAGENT_MAX_STEPS_KEY.to_string(), "80".to_string()),
            (TOPAGENT_MAX_RETRIES_KEY.to_string(), "6".to_string()),
            (TOPAGENT_TIMEOUT_SECS_KEY.to_string(), "100".to_string()),
        ]);

        let defaults = TelegramModeDefaults::from_metadata(&original_values);
        assert_eq!(defaults.api_key.as_deref(), Some("key-roundtrip"));
        assert_eq!(defaults.token.as_deref(), Some("111:token-roundtrip"));
        assert_eq!(defaults.model.as_deref(), Some("model/roundtrip"));
        assert_eq!(defaults.max_steps, Some(80));
        assert_eq!(defaults.max_retries, Some(6));
        assert_eq!(defaults.timeout_secs, Some(100));

        let params = CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();
        assert_eq!(config.openrouter_api_key.as_deref(), Some("key-roundtrip"));
        assert_eq!(config.token, "111:token-roundtrip");
        assert_eq!(config.route.model_id, "model/roundtrip");
        assert_eq!(config.options.max_steps, 80);
        assert_eq!(config.options.max_provider_retries, 6);
        assert_eq!(config.options.provider_timeout_secs, 100);
    }
}

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use topagent_core::{
    ProviderKind, RuntimeOptions,
    model::{DEFAULT_OPENROUTER_MODEL_ID, ModelRoute},
};

use crate::managed_files::read_managed_env_metadata;
use crate::operational_paths::managed_service_env_path;

pub(crate) const TELEGRAM_SERVICE_UNIT_NAME: &str = "topagent-telegram.service";
pub(crate) const TOPAGENT_SERVICE_MANAGED_KEY: &str = "TOPAGENT_SERVICE_MANAGED";
pub(crate) const TOPAGENT_WORKSPACE_KEY: &str = "TOPAGENT_WORKSPACE";
pub(crate) const TOPAGENT_MODEL_KEY: &str = "TOPAGENT_MODEL";
pub(crate) const TOPAGENT_TOOL_AUTHORING_KEY: &str = "TOPAGENT_TOOL_AUTHORING";
pub(crate) const TOPAGENT_MAX_STEPS_KEY: &str = "TOPAGENT_MAX_STEPS";
pub(crate) const TOPAGENT_MAX_RETRIES_KEY: &str = "TOPAGENT_MAX_RETRIES";
pub(crate) const TOPAGENT_TIMEOUT_SECS_KEY: &str = "TOPAGENT_TIMEOUT_SECS";
pub(crate) const OPENROUTER_API_KEY_KEY: &str = "OPENROUTER_API_KEY";
pub(crate) const OPENCODE_API_KEY_KEY: &str = "OPENCODE_API_KEY";

fn normalize_nonempty_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Shared CLI parameters threaded through install, service, telegram, and one-shot paths.
#[derive(Debug, Clone)]
pub(crate) struct CliParams {
    pub api_key: Option<String>,
    pub opencode_api_key: Option<String>,
    pub model: Option<String>,
    pub workspace: Option<PathBuf>,
    pub max_steps: Option<usize>,
    pub max_retries: Option<usize>,
    pub timeout_secs: Option<u64>,
    pub generated_tool_authoring: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct TelegramModeConfig {
    pub token: String,
    pub openrouter_api_key: Option<String>,
    pub opencode_api_key: Option<String>,
    pub route: ModelRoute,
    pub workspace: PathBuf,
    pub options: RuntimeOptions,
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

#[derive(Debug, Clone, Default)]
pub(crate) struct TelegramModeDefaults {
    pub api_key: Option<String>,
    pub opencode_api_key: Option<String>,
    pub token: Option<String>,
    pub workspace: Option<PathBuf>,
    pub model: Option<String>,
    pub max_steps: Option<usize>,
    pub max_retries: Option<usize>,
    pub timeout_secs: Option<u64>,
    pub generated_tool_authoring: Option<bool>,
}

impl TelegramModeDefaults {
    pub(crate) fn from_metadata(values: &HashMap<String, String>) -> Self {
        Self {
            api_key: normalize_nonempty_string(values.get(OPENROUTER_API_KEY_KEY).cloned()),
            opencode_api_key: normalize_nonempty_string(values.get(OPENCODE_API_KEY_KEY).cloned()),
            token: normalize_nonempty_string(values.get("TELEGRAM_BOT_TOKEN").cloned()),
            workspace: values.get(TOPAGENT_WORKSPACE_KEY).map(PathBuf::from),
            model: normalize_nonempty_string(values.get(TOPAGENT_MODEL_KEY).cloned()),
            max_steps: parse_optional_usize(values.get(TOPAGENT_MAX_STEPS_KEY).map(String::as_str)),
            max_retries: parse_optional_usize(
                values.get(TOPAGENT_MAX_RETRIES_KEY).map(String::as_str),
            ),
            timeout_secs: parse_optional_u64(
                values.get(TOPAGENT_TIMEOUT_SECS_KEY).map(String::as_str),
            ),
            generated_tool_authoring: parse_env_bool(
                values.get(TOPAGENT_TOOL_AUTHORING_KEY).map(String::as_str),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelResolutionSource {
    CliOverride,
    InteractiveSelection,
    PersistedDefault,
    BuiltInFallback,
}

impl ModelResolutionSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CliOverride => "CLI override",
            Self::InteractiveSelection => "interactive selection",
            Self::PersistedDefault => "persisted default",
            Self::BuiltInFallback => "built-in default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedModel {
    pub model_id: String,
    pub source: ModelResolutionSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeModelSelection {
    pub configured_default: ResolvedModel,
    pub effective: ResolvedModel,
}

pub(crate) fn build_runtime_options(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> RuntimeOptions {
    RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(3))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120))
}

pub(crate) fn build_runtime_options_with_defaults(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
    generated_tool_authoring: Option<bool>,
    defaults: &TelegramModeDefaults,
) -> RuntimeOptions {
    build_runtime_options(
        max_steps.or(defaults.max_steps),
        max_retries.or(defaults.max_retries),
        timeout_secs.or(defaults.timeout_secs),
    )
    .with_generated_tool_authoring(resolve_generated_tool_authoring(
        generated_tool_authoring,
        defaults.generated_tool_authoring,
    ))
}

pub(crate) fn resolve_generated_tool_authoring(
    requested: Option<bool>,
    persisted: Option<bool>,
) -> bool {
    requested.or(persisted).unwrap_or(false)
}

pub(crate) fn parse_env_bool(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value)
            if value.eq_ignore_ascii_case("1")
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("yes")
                || value.eq_ignore_ascii_case("on") =>
        {
            Some(true)
        }
        Some(value)
            if value.eq_ignore_ascii_case("0")
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("no")
                || value.eq_ignore_ascii_case("off") =>
        {
            Some(false)
        }
        _ => None,
    }
}

pub(crate) fn parse_optional_usize(value: Option<&str>) -> Option<usize> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
}

pub(crate) fn parse_optional_u64(value: Option<&str>) -> Option<u64> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
}

pub(crate) fn load_persisted_telegram_defaults() -> Result<TelegramModeDefaults> {
    let path = managed_service_env_path()?;
    let values = read_managed_env_metadata(&path).unwrap_or_default();
    Ok(TelegramModeDefaults::from_metadata(&values))
}

pub(crate) fn resolve_workspace_path(workspace: Option<PathBuf>) -> Result<PathBuf> {
    resolve_workspace_path_with_current_dir(workspace, std::env::current_dir())
}

pub(crate) fn resolve_workspace_path_with_current_dir(
    workspace: Option<PathBuf>,
    current_dir: std::io::Result<PathBuf>,
) -> Result<PathBuf> {
    let workspace = match workspace {
        Some(path) => path,
        None => current_dir.context(
            "Failed to determine the current directory. Run TopAgent from your repo or pass --workspace /path/to/repo.",
        )?,
    };

    if !workspace.exists() {
        return Err(anyhow::anyhow!(
            "Workspace path does not exist: {}. Run TopAgent from a repo directory or pass --workspace /path/to/repo.",
            workspace.display()
        ));
    }

    if !workspace.is_dir() {
        return Err(anyhow::anyhow!(
            "Workspace path is not a directory: {}",
            workspace.display()
        ));
    }

    workspace.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Workspace path is not accessible: {} ({})",
            workspace.display(),
            e
        )
    })
}

/// Resolve a required parameter from an explicit value or environment variable.
fn require_param(value: Option<String>, env_var: &str, missing_msg: &str) -> Result<String> {
    let resolved = value
        .and_then(|value| normalize_nonempty_string(Some(value)))
        .or_else(|| normalize_nonempty_string(std::env::var(env_var).ok()))
        .unwrap_or_default()
        .trim()
        .to_string();

    if resolved.is_empty() {
        return Err(anyhow::anyhow!("{}", missing_msg));
    }

    Ok(resolved)
}

pub(crate) fn require_openrouter_api_key(api_key: Option<String>) -> Result<String> {
    require_param(
        api_key,
        "OPENROUTER_API_KEY",
        "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY",
    )
}

pub(crate) fn require_opencode_api_key(api_key: Option<String>) -> Result<String> {
    require_param(
        api_key,
        "OPENCODE_API_KEY",
        "Opencode API key required: set --opencode-api-key or OPENCODE_API_KEY",
    )
}

pub(crate) fn resolve_provider_for_model(model_id: &str) -> ProviderKind {
    let lower = model_id.to_lowercase();
    if lower.starts_with("glm-")
        || lower.starts_with("mimo-")
        || lower.starts_with("minimax-m")
        || lower.starts_with("kimi-")
        || lower == "opencode"
    {
        ProviderKind::Opencode
    } else {
        ProviderKind::OpenRouter
    }
}

pub(crate) fn require_telegram_token(token: Option<String>) -> Result<String> {
    let token = require_param(
        token,
        "TELEGRAM_BOT_TOKEN",
        "Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN",
    )?;

    if !token.contains(':') {
        return Err(anyhow::anyhow!(
            "Telegram bot token looks invalid. Expected something like 123456:ABCdef..."
        ));
    }

    Ok(token)
}

pub(crate) fn require_telegram_token_with_default(
    token: Option<String>,
    persisted_default: Option<String>,
) -> Result<String> {
    require_telegram_token(token.or(persisted_default))
}

pub(crate) fn resolve_model_choice(
    explicit_model: Option<String>,
    interactive_selection: Option<String>,
    persisted_model: Option<String>,
) -> ResolvedModel {
    if let Some(model_id) = normalize_nonempty_string(explicit_model) {
        return ResolvedModel {
            model_id,
            source: ModelResolutionSource::CliOverride,
        };
    }

    if let Some(model_id) = normalize_nonempty_string(interactive_selection) {
        return ResolvedModel {
            model_id,
            source: ModelResolutionSource::InteractiveSelection,
        };
    }

    if let Some(model_id) = normalize_nonempty_string(persisted_model) {
        return ResolvedModel {
            model_id,
            source: ModelResolutionSource::PersistedDefault,
        };
    }

    ResolvedModel {
        model_id: DEFAULT_OPENROUTER_MODEL_ID.to_string(),
        source: ModelResolutionSource::BuiltInFallback,
    }
}

pub(crate) fn resolve_runtime_model_selection(
    explicit_model: Option<String>,
    persisted_model: Option<String>,
) -> RuntimeModelSelection {
    let configured_default = resolve_model_choice(None, None, persisted_model.clone());
    let effective = resolve_model_choice(explicit_model, None, persisted_model);
    RuntimeModelSelection {
        configured_default,
        effective,
    }
}

pub(crate) fn build_route_from_resolved(model: &ResolvedModel) -> ModelRoute {
    let provider = resolve_provider_for_model(&model.model_id);
    ModelRoute::new(provider, &model.model_id)
}

pub(crate) fn current_configured_model(persisted_model: Option<String>) -> ResolvedModel {
    resolve_model_choice(None, None, persisted_model)
}

pub(crate) fn resolve_telegram_mode_config(
    token: Option<String>,
    params: CliParams,
    defaults: TelegramModeDefaults,
) -> Result<TelegramModeConfig> {
    let model_selection = resolve_runtime_model_selection(params.model, defaults.model.clone());
    let route = build_route_from_resolved(&model_selection.effective);
    let openrouter_api_key = resolve_openrouter_api_key(params.api_key, &defaults)?;
    let opencode_api_key = resolve_opencode_api_key(params.opencode_api_key, &defaults)?;
    Ok(TelegramModeConfig {
        token: require_telegram_token_with_default(token, defaults.token.clone())?,
        openrouter_api_key,
        opencode_api_key,
        route,
        workspace: resolve_workspace_path(params.workspace.or_else(|| defaults.workspace.clone()))?,
        options: build_runtime_options_with_defaults(
            params.max_steps,
            params.max_retries,
            params.timeout_secs,
            params.generated_tool_authoring,
            &defaults,
        ),
    })
}

fn resolve_openrouter_api_key(
    cli_key: Option<String>,
    defaults: &TelegramModeDefaults,
) -> Result<Option<String>> {
    let key = require_openrouter_api_key(cli_key.or_else(|| defaults.api_key.clone())).ok();
    Ok(key)
}

fn resolve_opencode_api_key(
    cli_key: Option<String>,
    defaults: &TelegramModeDefaults,
) -> Result<Option<String>> {
    let key = require_opencode_api_key(cli_key.or_else(|| defaults.opencode_api_key.clone())).ok();
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_workspace_defaults_to_current_directory_for_one_shot_and_telegram() {
        let temp = TempDir::new().unwrap();
        let resolved =
            resolve_workspace_path_with_current_dir(None, Ok(temp.path().to_path_buf())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_override_beats_current_directory_for_one_shot_and_telegram() {
        let current = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(override_dir.path().to_path_buf()),
            Ok(current.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_resolution_fails_when_current_directory_is_unavailable() {
        let err = resolve_workspace_path_with_current_dir(
            None,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Failed to determine the current directory"));
    }

    #[test]
    fn test_workspace_override_ignores_invalid_current_directory() {
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(PathBuf::from(override_dir.path())),
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_generated_tool_authoring_prefers_requested_value() {
        assert!(!resolve_generated_tool_authoring(Some(false), Some(true)));
        assert!(resolve_generated_tool_authoring(Some(true), Some(false)));
    }

    #[test]
    fn test_resolve_generated_tool_authoring_falls_back_to_persisted_value() {
        assert!(resolve_generated_tool_authoring(None, Some(true)));
        assert!(!resolve_generated_tool_authoring(None, Some(false)));
        assert!(!resolve_generated_tool_authoring(None, None));
    }

    #[test]
    fn test_parse_env_bool_accepts_common_truthy_and_falsey_values() {
        assert_eq!(parse_env_bool(Some("1")), Some(true));
        assert_eq!(parse_env_bool(Some("true")), Some(true));
        assert_eq!(parse_env_bool(Some("on")), Some(true));
        assert_eq!(parse_env_bool(Some("0")), Some(false));
        assert_eq!(parse_env_bool(Some("false")), Some(false));
        assert_eq!(parse_env_bool(Some("off")), Some(false));
        assert_eq!(parse_env_bool(Some("unknown")), None);
        assert_eq!(parse_env_bool(None), None);
    }

    #[test]
    fn test_resolve_model_choice_prefers_explicit_then_selected_then_persisted() {
        let resolved = resolve_model_choice(
            Some(" explicit/model ".to_string()),
            Some("selected/model".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.model_id, "explicit/model");
        assert_eq!(resolved.source, ModelResolutionSource::CliOverride);

        let resolved = resolve_model_choice(
            Some("   ".to_string()),
            Some(" selected/model ".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.model_id, "selected/model");
        assert_eq!(resolved.source, ModelResolutionSource::InteractiveSelection);

        let resolved = resolve_model_choice(None, None, Some(" persisted/model ".to_string()));
        assert_eq!(resolved.model_id, "persisted/model");
        assert_eq!(resolved.source, ModelResolutionSource::PersistedDefault);

        let resolved = resolve_model_choice(None, None, None);
        assert_eq!(resolved.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(resolved.source, ModelResolutionSource::BuiltInFallback);
    }

    #[test]
    fn test_resolve_runtime_model_selection_tracks_configured_and_effective_models() {
        let resolved = resolve_runtime_model_selection(
            Some(" explicit/model ".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.configured_default.model_id, "persisted/model");
        assert_eq!(
            resolved.configured_default.source,
            ModelResolutionSource::PersistedDefault
        );
        assert_eq!(resolved.effective.model_id, "explicit/model");
        assert_eq!(
            resolved.effective.source,
            ModelResolutionSource::CliOverride
        );

        let resolved = resolve_runtime_model_selection(None, None);
        assert_eq!(
            resolved.configured_default.model_id,
            DEFAULT_OPENROUTER_MODEL_ID
        );
        assert_eq!(resolved.effective.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(
            resolved.configured_default.source,
            ModelResolutionSource::BuiltInFallback
        );
        assert_eq!(
            resolved.effective.source,
            ModelResolutionSource::BuiltInFallback
        );
    }

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
            generated_tool_authoring: None,
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
            generated_tool_authoring: None,
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
            (TOPAGENT_TOOL_AUTHORING_KEY.to_string(), "1".to_string()),
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
            generated_tool_authoring: None,
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();

        assert_eq!(config.openrouter_api_key.as_deref(), Some("persisted-key"));
        assert_eq!(config.token, "123456:abcdef");
        assert_eq!(config.route.model_id, "persisted/model");
        assert_eq!(config.workspace, workspace.path().canonicalize().unwrap());
        assert_eq!(config.options.max_steps, 61);
        assert_eq!(config.options.max_provider_retries, 4);
        assert_eq!(config.options.provider_timeout_secs, 75);
        assert!(config.options.enable_generated_tool_authoring);
    }

    #[test]
    fn test_build_route_from_resolved_uses_resolved_model_id() {
        let resolved = ResolvedModel {
            model_id: "custom/model".to_string(),
            source: ModelResolutionSource::CliOverride,
        };

        assert_eq!(
            build_route_from_resolved(&resolved).model_id,
            "custom/model"
        );
    }

    #[test]
    fn test_current_configured_model_uses_persisted_then_built_in_fallback() {
        let configured = current_configured_model(Some("persisted/model".to_string()));
        assert_eq!(configured.model_id, "persisted/model");
        assert_eq!(configured.source, ModelResolutionSource::PersistedDefault);

        let fallback = current_configured_model(None);
        assert_eq!(fallback.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(fallback.source, ModelResolutionSource::BuiltInFallback);
    }

    #[test]
    fn test_require_persisted_defaults_accept_trimmed_values() {
        assert_eq!(
            require_openrouter_api_key(Some(" test-key ".to_string())).unwrap(),
            "test-key"
        );
        assert_eq!(
            require_telegram_token_with_default(None, Some("123456:abcdef ".to_string())).unwrap(),
            "123456:abcdef"
        );
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
            (TOPAGENT_TOOL_AUTHORING_KEY.to_string(), "1".to_string()),
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
            generated_tool_authoring: None,
        };

        let config = resolve_telegram_mode_config(None, params.clone(), defaults.clone()).unwrap();
        assert_eq!(config.openrouter_api_key.as_deref(), Some("shared-key"));
        assert_eq!(config.token, "123456:shared-token");
        assert_eq!(config.route.model_id, "shared/model");
        assert_eq!(config.workspace, workspace.path().canonicalize().unwrap());
        assert_eq!(config.options.max_steps, 65);
        assert_eq!(config.options.max_provider_retries, 5);
        assert_eq!(config.options.provider_timeout_secs, 90);
        assert!(config.options.enable_generated_tool_authoring);

        let selection = resolve_runtime_model_selection(None, defaults.model.clone());
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
            generated_tool_authoring: None,
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
            (TOPAGENT_TOOL_AUTHORING_KEY.to_string(), "1".to_string()),
        ]);

        let defaults = TelegramModeDefaults::from_metadata(&original_values);
        assert_eq!(defaults.api_key.as_deref(), Some("key-roundtrip"));
        assert_eq!(defaults.token.as_deref(), Some("111:token-roundtrip"));
        assert_eq!(defaults.model.as_deref(), Some("model/roundtrip"));
        assert_eq!(defaults.max_steps, Some(80));
        assert_eq!(defaults.max_retries, Some(6));
        assert_eq!(defaults.timeout_secs, Some(100));
        assert_eq!(defaults.generated_tool_authoring, Some(true));

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

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();
        assert_eq!(config.openrouter_api_key.as_deref(), Some("key-roundtrip"));
        assert_eq!(config.token, "111:token-roundtrip");
        assert_eq!(config.route.model_id, "model/roundtrip");
        assert_eq!(config.options.max_steps, 80);
        assert_eq!(config.options.max_provider_retries, 6);
        assert_eq!(config.options.provider_timeout_secs, 100);
        assert!(config.options.enable_generated_tool_authoring);
    }

    #[test]
    fn test_empty_persisted_model_falls_back_to_built_in_default() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "some-key".to_string()),
            ("TELEGRAM_BOT_TOKEN".to_string(), "123:token".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (TOPAGENT_MODEL_KEY.to_string(), "   ".to_string()),
        ]);

        let defaults = TelegramModeDefaults::from_metadata(&values);
        assert!(
            defaults.model.is_none(),
            "whitespace-only model should parse as None"
        );

        let selection = resolve_runtime_model_selection(None, defaults.model);
        assert_eq!(
            selection.configured_default.model_id,
            DEFAULT_OPENROUTER_MODEL_ID
        );
        assert_eq!(selection.effective.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }
}

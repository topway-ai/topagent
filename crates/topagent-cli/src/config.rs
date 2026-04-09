use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use topagent_core::{
    model::{ModelRoute, DEFAULT_OPENROUTER_MODEL_ID},
    RuntimeOptions,
};

use crate::managed_files::read_managed_env_metadata;

pub(crate) const TELEGRAM_SERVICE_UNIT_NAME: &str = "topagent-telegram.service";
pub(crate) const TOPAGENT_SERVICE_MANAGED_KEY: &str = "TOPAGENT_SERVICE_MANAGED";
pub(crate) const TOPAGENT_WORKSPACE_KEY: &str = "TOPAGENT_WORKSPACE";
pub(crate) const TOPAGENT_MODEL_KEY: &str = "TOPAGENT_MODEL";
pub(crate) const TOPAGENT_TOOL_AUTHORING_KEY: &str = "TOPAGENT_TOOL_AUTHORING";
pub(crate) const TOPAGENT_MAX_STEPS_KEY: &str = "TOPAGENT_MAX_STEPS";
pub(crate) const TOPAGENT_MAX_RETRIES_KEY: &str = "TOPAGENT_MAX_RETRIES";
pub(crate) const TOPAGENT_TIMEOUT_SECS_KEY: &str = "TOPAGENT_TIMEOUT_SECS";
const TOPAGENT_MANAGED_ENV_RELATIVE_PATH: &str = "topagent/services/topagent-telegram.env";

fn normalize_nonempty_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Shared CLI parameters threaded through install, service, telegram, and one-shot paths.
#[derive(Debug, Clone)]
pub(crate) struct CliParams {
    pub api_key: Option<String>,
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
    pub api_key: String,
    pub route: ModelRoute,
    pub workspace: PathBuf,
    pub options: RuntimeOptions,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TelegramModeDefaults {
    pub api_key: Option<String>,
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
            api_key: normalize_nonempty_string(values.get("OPENROUTER_API_KEY").cloned()),
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

pub(crate) fn resolve_config_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine your config directory. Set XDG_CONFIG_HOME or HOME first."
            )
        })?;
    Ok(home.join(".config"))
}

pub(crate) fn managed_service_env_path() -> Result<PathBuf> {
    Ok(resolve_config_home()?.join(TOPAGENT_MANAGED_ENV_RELATIVE_PATH))
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

pub(crate) fn require_openrouter_api_key_with_default(
    api_key: Option<String>,
    persisted_default: Option<String>,
) -> Result<String> {
    require_openrouter_api_key(api_key.or(persisted_default))
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
    ModelRoute::openrouter(&model.model_id)
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
    Ok(TelegramModeConfig {
        token: require_telegram_token_with_default(token, defaults.token.clone())?,
        api_key: require_openrouter_api_key_with_default(params.api_key, defaults.api_key.clone())?,
        route: build_route_from_resolved(&model_selection.effective),
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
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };

        let config = resolve_telegram_mode_config(None, params, defaults).unwrap();

        assert_eq!(config.api_key, "persisted-key");
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
            require_openrouter_api_key_with_default(None, Some(" test-key ".to_string())).unwrap(),
            "test-key"
        );
        assert_eq!(
            require_telegram_token_with_default(None, Some("123456:abcdef ".to_string())).unwrap(),
            "123456:abcdef"
        );
    }
}

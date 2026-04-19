use std::path::PathBuf;
use std::collections::HashMap;

use crate::managed_files::read_managed_env_metadata;
use crate::operational_paths::managed_service_env_path;
use crate::config::model_selection::{SelectedProvider, canonicalize_allowed_username};

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
pub(crate) const TELEGRAM_BOT_TOKEN_KEY: &str = "TELEGRAM_BOT_TOKEN";
pub(crate) const TELEGRAM_ALLOWED_DM_USERNAME_KEY: &str = "TELEGRAM_ALLOWED_DM_USERNAME";
pub(crate) const TELEGRAM_BOUND_DM_USER_ID_KEY: &str = "TELEGRAM_BOUND_DM_USER_ID";
pub(crate) const TOPAGENT_PROVIDER_KEY: &str = "TOPAGENT_PROVIDER";

pub(crate) fn normalize_nonempty_string(value: Option<String>) -> Option<String> {
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
    pub provider: Option<SelectedProvider>,
    pub allowed_dm_username: Option<String>,
    pub bound_dm_user_id: Option<i64>,
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
            provider: values
                .get(TOPAGENT_PROVIDER_KEY)
                .and_then(|v| SelectedProvider::from_str(v)),
            allowed_dm_username: values
                .get(TELEGRAM_ALLOWED_DM_USERNAME_KEY)
                .and_then(|v| canonicalize_allowed_username(v)),
            bound_dm_user_id: values
                .get(TELEGRAM_BOUND_DM_USER_ID_KEY)
                .and_then(|v| v.parse().ok()),
        }
    }
}

pub(crate) fn load_persisted_telegram_defaults() -> anyhow::Result<TelegramModeDefaults> {
    let path = managed_service_env_path()?;
    let values = read_managed_env_metadata(&path).unwrap_or_default();
    Ok(TelegramModeDefaults::from_metadata(&values))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_telegram_mode_defaults_canonicalize_allowed_username_on_read() {
        use std::collections::HashMap;
        let values = HashMap::from([(
            TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
            "@MixedCase".to_string(),
        )]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        assert_eq!(defaults.allowed_dm_username.as_deref(), Some("mixedcase"));
    }
}

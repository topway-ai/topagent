use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::config::{
    TelegramModeConfig, OPENCODE_API_KEY_KEY, OPENROUTER_API_KEY_KEY,
    TELEGRAM_ALLOWED_DM_USERNAME_KEY, TELEGRAM_BOT_TOKEN_KEY, TELEGRAM_BOUND_DM_USER_ID_KEY,
    TOPAGENT_MAX_RETRIES_KEY, TOPAGENT_MAX_STEPS_KEY, TOPAGENT_MODEL_KEY, TOPAGENT_PROVIDER_KEY,
    TOPAGENT_SERVICE_MANAGED_KEY, TOPAGENT_TIMEOUT_SECS_KEY, TOPAGENT_TOOL_AUTHORING_KEY,
    TOPAGENT_WORKSPACE_KEY,
};
use crate::managed_files::{write_managed_file, TOPAGENT_MANAGED_HEADER};

pub(super) fn render_service_env_file(config: &TelegramModeConfig) -> Result<String> {
    let workspace = config.workspace.display().to_string();
    for value in [
        config.token.as_str(),
        workspace.as_str(),
        config.route.model_id.as_str(),
    ] {
        ensure_env_value_is_safe(value)?;
    }
    if let Some(ref key) = config.openrouter_api_key {
        ensure_env_value_is_safe(key)?;
    }
    if let Some(ref key) = config.opencode_api_key {
        ensure_env_value_is_safe(key)?;
    }
    if let Some(ref username) = config.allowed_dm_username {
        ensure_env_value_is_safe(username)?;
    }

    let opencode_line = config
        .opencode_api_key
        .as_ref()
        .map(|key| format!("{}={}\n", OPENCODE_API_KEY_KEY, quote_env_value(key)))
        .unwrap_or_default();
    let allowed_username_line = config
        .allowed_dm_username
        .as_ref()
        .map(|name| {
            format!(
                "{}={}\n",
                TELEGRAM_ALLOWED_DM_USERNAME_KEY,
                quote_env_value(name)
            )
        })
        .unwrap_or_default();
    let bound_user_id_line = config
        .bound_dm_user_id
        .map(|id| {
            format!(
                "{}={}\n",
                TELEGRAM_BOUND_DM_USER_ID_KEY,
                quote_env_value(&id.to_string())
            )
        })
        .unwrap_or_default();

    Ok(format!(
        "{header}
{managed_key}=1
{provider_key}={provider}
{token_key}={token}
{api_key_key}={api_key}
{opencode_line}{workspace_key}={workspace}
{model_key}={model}
{max_steps_key}={max_steps}
{max_retries_key}={max_retries}
{timeout_secs_key}={timeout_secs}
{tool_authoring_key}={tool_authoring}
{allowed_username_line}{bound_user_id_line}",
        header = TOPAGENT_MANAGED_HEADER,
        managed_key = TOPAGENT_SERVICE_MANAGED_KEY,
        provider_key = TOPAGENT_PROVIDER_KEY,
        provider = quote_env_value(config.selected_provider.label()),
        token = quote_env_value(&config.token),
        api_key = quote_env_value(config.openrouter_api_key.as_deref().unwrap_or("")),
        opencode_line = opencode_line,
        workspace_key = TOPAGENT_WORKSPACE_KEY,
        workspace = quote_env_value(&workspace),
        model_key = TOPAGENT_MODEL_KEY,
        model = quote_env_value(&config.route.model_id),
        max_steps_key = TOPAGENT_MAX_STEPS_KEY,
        max_steps = quote_env_value(&config.options.max_steps.to_string()),
        max_retries_key = TOPAGENT_MAX_RETRIES_KEY,
        max_retries = quote_env_value(&config.options.max_provider_retries.to_string()),
        timeout_secs_key = TOPAGENT_TIMEOUT_SECS_KEY,
        timeout_secs = quote_env_value(&config.options.provider_timeout_secs.to_string()),
        tool_authoring_key = TOPAGENT_TOOL_AUTHORING_KEY,
        tool_authoring = quote_env_value(if config.options.enable_generated_tool_authoring {
            "1"
        } else {
            "0"
        }),
        allowed_username_line = allowed_username_line,
        bound_user_id_line = bound_user_id_line,
        api_key_key = OPENROUTER_API_KEY_KEY,
        token_key = TELEGRAM_BOT_TOKEN_KEY,
    ))
}

pub(crate) fn write_managed_env_values(
    path: &Path,
    values: &HashMap<String, String>,
) -> Result<()> {
    let contents = render_managed_env_values(values)?;
    write_managed_file(path, &contents, true)
}

pub(super) fn trim_nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn persisted_model_from_env_values(values: &HashMap<String, String>) -> Option<String> {
    values
        .get(TOPAGENT_MODEL_KEY)
        .map(String::to_string)
        .and_then(|value| trim_nonempty(Some(value)))
}

fn render_managed_env_values(values: &HashMap<String, String>) -> Result<String> {
    let mut entries: Vec<_> = values.iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut rendered = String::new();
    rendered.push_str(TOPAGENT_MANAGED_HEADER);
    rendered.push('\n');
    for (key, value) in entries {
        ensure_env_value_is_safe(key)?;
        ensure_env_value_is_safe(value)?;
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&quote_env_value(value));
        rendered.push('\n');
    }

    Ok(rendered)
}

fn ensure_env_value_is_safe(value: &str) -> Result<()> {
    if value.contains('\n') {
        return Err(anyhow::anyhow!(
            "Service configuration contains a newline, which cannot be written safely."
        ));
    }
    Ok(())
}

fn quote_env_value(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '$' => quoted.push_str("\\$"),
            '`' => quoted.push_str("\\`"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_service_env_file_round_trips_operator_config_in_one_write() {
        use crate::config::{SelectedProvider, TelegramModeConfig, TelegramModeDefaults};
        use std::path::PathBuf;
        use topagent_core::{model::ModelRoute, ProviderKind, RuntimeOptions};

        let workspace = PathBuf::from("/tmp/topagent-roundtrip-workspace");
        let config = TelegramModeConfig {
            token: "111:roundtrip-token".to_string(),
            openrouter_api_key: Some("sk-operator-entered".to_string()),
            opencode_api_key: Some("opencode-operator-entered".to_string()),
            configured_default_model: "minimax/minimax-m2.7".to_string(),
            route: ModelRoute::new(ProviderKind::OpenRouter, "minimax/minimax-m2.7"),
            workspace: workspace.clone(),
            options: RuntimeOptions::new()
                .with_max_steps(55)
                .with_max_provider_retries(4)
                .with_provider_timeout_secs(95)
                .with_generated_tool_authoring(true),
            selected_provider: SelectedProvider::OpenRouter,
            allowed_dm_username: Some("operator".to_string()),
            bound_dm_user_id: Some(8675309),
        };

        let rendered = render_service_env_file(&config).unwrap();

        // Critical operator-entered secrets must be present after a single
        // render (the install path must not need a follow-up write to add
        // provider, allowed username, or bound user id).
        assert!(rendered.contains("OPENROUTER_API_KEY=\"sk-operator-entered\""));
        assert!(rendered.contains("OPENCODE_API_KEY=\"opencode-operator-entered\""));
        assert!(rendered.contains("TELEGRAM_BOT_TOKEN=\"111:roundtrip-token\""));
        assert!(rendered.contains("TOPAGENT_PROVIDER=\"OpenRouter\""));
        assert!(rendered.contains("TELEGRAM_ALLOWED_DM_USERNAME=\"operator\""));
        assert!(rendered.contains("TELEGRAM_BOUND_DM_USER_ID=\"8675309\""));
        assert!(rendered.contains("TOPAGENT_MODEL=\"minimax/minimax-m2.7\""));

        // Parse the rendered file back through the same metadata reader the
        // service uses on startup. The defaults must round-trip exactly.
        let parsed = parse_env_file(&rendered);
        let defaults = TelegramModeDefaults::from_metadata(&parsed);

        assert_eq!(defaults.api_key.as_deref(), Some("sk-operator-entered"));
        assert_eq!(
            defaults.opencode_api_key.as_deref(),
            Some("opencode-operator-entered")
        );
        assert_eq!(defaults.token.as_deref(), Some("111:roundtrip-token"));
        assert_eq!(defaults.model.as_deref(), Some("minimax/minimax-m2.7"));
        assert_eq!(defaults.max_steps, Some(55));
        assert_eq!(defaults.max_retries, Some(4));
        assert_eq!(defaults.timeout_secs, Some(95));
        assert_eq!(defaults.generated_tool_authoring, Some(true));
        assert_eq!(defaults.provider, Some(SelectedProvider::OpenRouter));
        assert_eq!(defaults.allowed_dm_username.as_deref(), Some("operator"));
        assert_eq!(defaults.bound_dm_user_id, Some(8675309));
    }

    #[test]
    fn test_render_service_env_file_omits_optional_lines_when_unset() {
        use crate::config::{SelectedProvider, TelegramModeConfig};
        use std::path::PathBuf;
        use topagent_core::{model::ModelRoute, ProviderKind, RuntimeOptions};

        let config = TelegramModeConfig {
            token: "1:t".to_string(),
            openrouter_api_key: Some("sk".to_string()),
            opencode_api_key: None,
            configured_default_model: "m".to_string(),
            route: ModelRoute::new(ProviderKind::OpenRouter, "m"),
            workspace: PathBuf::from("/tmp/ws"),
            options: RuntimeOptions::default(),
            selected_provider: SelectedProvider::OpenRouter,
            allowed_dm_username: None,
            bound_dm_user_id: None,
        };
        let rendered = render_service_env_file(&config).unwrap();
        assert!(!rendered.contains("OPENCODE_API_KEY"));
        assert!(!rendered.contains("TELEGRAM_ALLOWED_DM_USERNAME"));
        assert!(!rendered.contains("TELEGRAM_BOUND_DM_USER_ID"));
    }

    /// Minimal env-file parser for tests: handles the quoted shell-style
    /// `KEY="value"` lines emitted by `render_service_env_file`.
    fn parse_env_file(rendered: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for raw in rendered.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            map.insert(key.to_string(), value.to_string());
        }
        map
    }

    #[test]
    fn test_render_managed_env_values_keeps_all_existing_entries() {
        let values = HashMap::from([
            (TOPAGENT_SERVICE_MANAGED_KEY.to_string(), "1".to_string()),
            (OPENROUTER_API_KEY_KEY.to_string(), "key".to_string()),
            (TELEGRAM_BOT_TOKEN_KEY.to_string(), "123:abc".to_string()),
            (
                TOPAGENT_MODEL_KEY.to_string(),
                "qwen/qwen3.6-plus".to_string(),
            ),
            ("EXTRA_FLAG".to_string(), "still-here".to_string()),
        ]);

        let rendered = render_managed_env_values(&values).unwrap();

        assert!(rendered.contains("TOPAGENT_SERVICE_MANAGED=\"1\""));
        assert!(rendered.contains("OPENROUTER_API_KEY=\"key\""));
        assert!(rendered.contains("TELEGRAM_BOT_TOKEN=\"123:abc\""));
        assert!(rendered.contains("TOPAGENT_MODEL=\"qwen/qwen3.6-plus\""));
        assert!(rendered.contains("EXTRA_FLAG=\"still-here\""));
    }
}

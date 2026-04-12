use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::config::{
    TelegramModeConfig, TOPAGENT_MAX_RETRIES_KEY, TOPAGENT_MAX_STEPS_KEY, TOPAGENT_MODEL_KEY,
    TOPAGENT_SERVICE_MANAGED_KEY, TOPAGENT_TIMEOUT_SECS_KEY, TOPAGENT_TOOL_AUTHORING_KEY,
    TOPAGENT_WORKSPACE_KEY,
};
use crate::managed_files::{write_managed_file, TOPAGENT_MANAGED_HEADER};

pub(crate) const OPENROUTER_API_KEY_KEY: &str = "OPENROUTER_API_KEY";
pub(crate) const TELEGRAM_BOT_TOKEN_KEY: &str = "TELEGRAM_BOT_TOKEN";

pub(super) fn render_service_env_file(config: &TelegramModeConfig) -> Result<String> {
    let workspace = config.workspace.display().to_string();
    for value in [
        config.token.as_str(),
        config.api_key.as_str(),
        workspace.as_str(),
        config.route.model_id.as_str(),
    ] {
        ensure_env_value_is_safe(value)?;
    }

    Ok(format!(
        "{header}
{managed_key}=1
{token_key}={token}
{api_key_key}={api_key}
{workspace_key}={workspace}
{model_key}={model}
{max_steps_key}={max_steps}
{max_retries_key}={max_retries}
{timeout_secs_key}={timeout_secs}
{tool_authoring_key}={tool_authoring}
",
        header = TOPAGENT_MANAGED_HEADER,
        managed_key = TOPAGENT_SERVICE_MANAGED_KEY,
        token = quote_env_value(&config.token),
        api_key = quote_env_value(&config.api_key),
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
        api_key_key = OPENROUTER_API_KEY_KEY,
        token_key = TELEGRAM_BOT_TOKEN_KEY,
    ))
}

pub(super) fn write_managed_env_values(
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

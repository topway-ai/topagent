use crate::config::defaults::normalize_nonempty_string;

/// Resolve a required parameter from an explicit value or environment variable.
fn require_param(value: Option<String>, env_var: &str, missing_msg: &str) -> anyhow::Result<String> {
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

pub(crate) fn require_openrouter_api_key(api_key: Option<String>) -> anyhow::Result<String> {
    require_param(
        api_key,
        "OPENROUTER_API_KEY",
        "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY",
    )
}

pub(crate) fn require_opencode_api_key(api_key: Option<String>) -> anyhow::Result<String> {
    require_param(
        api_key,
        "OPENCODE_API_KEY",
        "Opencode API key required: set --opencode-api-key or OPENCODE_API_KEY",
    )
}

pub(crate) fn require_telegram_token(token: Option<String>) -> anyhow::Result<String> {
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
) -> anyhow::Result<String> {
    require_telegram_token(token.or(persisted_default))
}

/// Resolve the OpenRouter API key from CLI flag, then persisted defaults.
/// Returns None when no key is available (soft failure; callers that need
/// the key use `require_openrouter_api_key` for a hard failure).
pub(crate) fn resolve_openrouter_api_key(
    cli_key: Option<String>,
    defaults_api_key: Option<&str>,
) -> Option<String> {
    require_openrouter_api_key(cli_key.or_else(|| defaults_api_key.map(str::to_string))).ok()
}

/// Resolve the Opencode API key from CLI flag, then persisted defaults.
pub(crate) fn resolve_opencode_api_key(
    cli_key: Option<String>,
    defaults_api_key: Option<&str>,
) -> Option<String> {
    require_opencode_api_key(cli_key.or_else(|| defaults_api_key.map(str::to_string))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

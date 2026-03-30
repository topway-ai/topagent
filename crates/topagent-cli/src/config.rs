use anyhow::{Context, Result};
use std::path::PathBuf;
use topagent_core::{
    model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory},
    RuntimeOptions,
};

pub(crate) const TELEGRAM_SERVICE_UNIT_NAME: &str = "topagent-telegram.service";
pub(crate) const TOPAGENT_SERVICE_MANAGED_KEY: &str = "TOPAGENT_SERVICE_MANAGED";
pub(crate) const TOPAGENT_WORKSPACE_KEY: &str = "TOPAGENT_WORKSPACE";
pub(crate) const TOPAGENT_PROVIDER_KEY: &str = "TOPAGENT_PROVIDER";
pub(crate) const TOPAGENT_MODEL_KEY: &str = "TOPAGENT_MODEL";
pub(crate) const OPENROUTER_API_KEY_KEY: &str = "OPENROUTER_API_KEY";
pub(crate) const TELEGRAM_BOT_TOKEN_KEY: &str = "TELEGRAM_BOT_TOKEN";
pub(crate) const TELEGRAM_HISTORY_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub(crate) struct TelegramModeConfig {
    pub token: String,
    pub api_key: String,
    pub route: ModelRoute,
    pub workspace: PathBuf,
    pub options: RuntimeOptions,
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

pub(crate) fn require_openrouter_api_key(api_key: Option<String>) -> Result<String> {
    let api_key = api_key
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if api_key.is_empty() {
        return Err(anyhow::anyhow!(
            "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY"
        ));
    }

    Ok(api_key)
}

pub(crate) fn require_telegram_token(token: Option<String>) -> Result<String> {
    let token = token
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN"
        ));
    }

    if !token.contains(':') {
        return Err(anyhow::anyhow!(
            "Telegram bot token looks invalid. Expected something like 123456:ABCdef..."
        ));
    }

    Ok(token)
}

pub(crate) fn build_route(provider: String, model: Option<String>) -> Result<ModelRoute> {
    let provider_id = ProviderId::parse(&provider).map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut route = RoutingPolicy::select_route(TaskCategory::Default, model.as_deref());
    route.provider_id = provider_id;
    Ok(route)
}

pub(crate) fn resolve_telegram_mode_config(
    token: Option<String>,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<TelegramModeConfig> {
    Ok(TelegramModeConfig {
        token: require_telegram_token(token)?,
        api_key: require_openrouter_api_key(api_key)?,
        route: build_route(provider, model)?,
        workspace: resolve_workspace_path(workspace)?,
        options: build_runtime_options(max_steps, max_retries, timeout_secs),
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
}

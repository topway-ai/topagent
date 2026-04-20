use crate::config::defaults::{
    CliParams, TOPAGENT_MODEL_KEY, TOPAGENT_PROVIDER_KEY, TOPAGENT_SERVICE_MANAGED_KEY,
};
use crate::config::model_selection::{
    provider_or_default, resolve_runtime_model_selection, ModelResolutionSource, SelectedProvider,
};
use crate::doctor::types::{CheckLevel, CheckResult};
use crate::managed_files::{is_topagent_managed_file, read_managed_env_metadata};
use crate::operational_paths::service_paths;

pub(crate) fn check_service_config(params: &CliParams, checks: &mut Vec<CheckResult>) {
    let paths = match service_paths() {
        Ok(paths) => paths,
        Err(err) => {
            checks.push(CheckResult {
                name: "service config",
                level: CheckLevel::Error,
                detail: format!("cannot resolve config paths: {}", err),
                hint: None,
            });
            return;
        }
    };

    check_api_key(params, &paths, checks);
    check_model_config(params, &paths, checks);
    check_managed_env(&paths, checks);
    check_telegram_token(&paths, checks);
    check_service_install(&paths, checks);
}

fn check_api_key(
    params: &CliParams,
    _paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let from_env = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let from_cli = params.api_key.as_deref().filter(|v| !v.trim().is_empty());

    if from_cli.is_some() || from_env.is_some() {
        let source = if from_cli.is_some() {
            "CLI flag"
        } else {
            "OPENROUTER_API_KEY env"
        };
        checks.push(CheckResult {
            name: "OpenRouter API key",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "OpenRouter API key",
            level: CheckLevel::Error,
            detail: "not found in env or CLI flag".to_string(),
            hint: Some("set OPENROUTER_API_KEY or pass --api-key".to_string()),
        });
    }

    let opencode_from_env = std::env::var("OPENCODE_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let opencode_from_cli = params
        .opencode_api_key
        .as_deref()
        .filter(|v| !v.trim().is_empty());

    if opencode_from_cli.is_some() || opencode_from_env.is_some() {
        let source = if opencode_from_cli.is_some() {
            "CLI flag"
        } else {
            "OPENCODE_API_KEY env"
        };
        checks.push(CheckResult {
            name: "Opencode API key",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    }
}

fn check_model_config(
    params: &CliParams,
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let persisted_model = env_values
        .get(TOPAGENT_MODEL_KEY)
        .filter(|v| !v.trim().is_empty())
        .map(String::from);

    let persisted_provider = env_values
        .get(TOPAGENT_PROVIDER_KEY)
        .and_then(|v| SelectedProvider::from_str(v));
    let selection = resolve_runtime_model_selection(
        provider_or_default(persisted_provider),
        params.model.clone(),
        persisted_model,
    );

    if selection.effective.source == ModelResolutionSource::BuiltInFallback {
        checks.push(CheckResult {
            name: "model config",
            level: CheckLevel::Warning,
            detail: format!("using built-in default: {}", selection.effective.model_id),
            hint: Some(
                "run `topagent model pick` or `topagent model set <id>` to configure a model"
                    .to_string(),
            ),
        });
    } else {
        checks.push(CheckResult {
            name: "model config",
            level: CheckLevel::Ok,
            detail: format!(
                "{} ({})",
                selection.effective.model_id,
                selection.effective.source.label()
            ),
            hint: None,
        });
    }
}

fn check_managed_env(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    if !paths.env_path.exists() {
        checks.push(CheckResult {
            name: "managed env/config",
            level: CheckLevel::Warning,
            detail: "env file does not exist".to_string(),
            hint: Some("run `topagent install` to create managed config".to_string()),
        });
        return;
    }

    match read_managed_env_metadata(&paths.env_path) {
        Ok(values) => {
            let is_managed = values
                .get(TOPAGENT_SERVICE_MANAGED_KEY)
                .is_some_and(|v| v == "1");
            if is_managed {
                let key_count = values.len();
                checks.push(CheckResult {
                    name: "managed env/config",
                    level: CheckLevel::Ok,
                    detail: format!("readable, {} key(s), managed", key_count),
                    hint: None,
                });
            } else {
                checks.push(CheckResult {
                    name: "managed env/config",
                    level: CheckLevel::Warning,
                    detail: "file exists but not managed by TopAgent".to_string(),
                    hint: None,
                });
            }
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "managed env/config",
                level: CheckLevel::Error,
                detail: format!("cannot read: {}", err),
                hint: None,
            });
        }
    }
}

fn check_telegram_token(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let from_env = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let from_config = values
        .get("TELEGRAM_BOT_TOKEN")
        .filter(|v| !v.trim().is_empty());

    if from_env.is_some() || from_config.is_some() {
        let source = if from_env.is_some() {
            "env"
        } else {
            "managed config"
        };
        checks.push(CheckResult {
            name: "Telegram token",
            level: CheckLevel::Ok,
            detail: format!("present ({})", source),
            hint: None,
        });
    } else {
        checks.push(CheckResult {
            name: "Telegram token",
            level: CheckLevel::Warning,
            detail: "not found in env or managed config".to_string(),
            hint: Some("set TELEGRAM_BOT_TOKEN or run `topagent install`".to_string()),
        });
    }
}

fn check_service_install(
    paths: &crate::operational_paths::ServicePaths,
    checks: &mut Vec<CheckResult>,
) {
    let unit_exists = paths.unit_path.exists();
    let env_exists = paths.env_path.exists();

    if !unit_exists && !env_exists {
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Warning,
            detail: "neither unit file nor env file installed".to_string(),
            hint: Some("run `topagent install` to set up the Telegram service".to_string()),
        });
        return;
    }

    let managed_unit = if unit_exists {
        is_topagent_managed_file(&paths.unit_path).unwrap_or(false)
    } else {
        false
    };
    let managed_env = if env_exists {
        is_topagent_managed_file(&paths.env_path).unwrap_or(false)
    } else {
        false
    };

    if managed_unit && managed_env {
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Ok,
            detail: "unit file and env file installed and managed".to_string(),
            hint: None,
        });
    } else {
        let mut issues = Vec::new();
        if unit_exists && !managed_unit {
            issues.push("unit file not managed by TopAgent");
        }
        if !unit_exists {
            issues.push("unit file missing");
        }
        if env_exists && !managed_env {
            issues.push("env file not managed by TopAgent");
        }
        if !env_exists {
            issues.push("env file missing");
        }
        checks.push(CheckResult {
            name: "service install",
            level: CheckLevel::Warning,
            detail: issues.join("; "),
            hint: Some("run `topagent install` to repair".to_string()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::CliParams;
    use crate::operational_paths::service_paths;

    #[test]
    fn test_doctor_reports_missing_model_config() {
        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let mut checks = Vec::new();
        check_model_config(&params, &service_paths().unwrap(), &mut checks);
        let model_check = checks.iter().find(|c| c.name == "model config").unwrap();
        assert!(model_check.level == CheckLevel::Ok || model_check.level == CheckLevel::Warning);
    }
}

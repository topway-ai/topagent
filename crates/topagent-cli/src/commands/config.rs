use anyhow::Result;

use crate::config::defaults::CliParams;
use crate::config::defaults::load_persisted_telegram_defaults;
use crate::config::summary::{resolve_contract_summary, ResolvedContractSummary};

pub(crate) fn run_config_inspect(params: CliParams) -> Result<()> {
    let defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let summary = resolve_contract_summary(&params, &defaults);
    print!("{}", render_contract_summary(&summary));
    Ok(())
}

pub(crate) fn render_contract_summary(summary: &ResolvedContractSummary) -> String {
    let mut out = String::from("TopAgent runtime contract\n\n");

    out.push_str(&format!("Provider:           {}\n", summary.provider));
    out.push_str(&format!(
        "Model:              {}  [{}]\n",
        summary.effective_model, summary.effective_model_source_label
    ));
    if let Some(ref default_model) = summary.configured_default_model {
        out.push_str(&format!(
            "Default model:      {}  [configured default]\n",
            default_model
        ));
    }
    match &summary.workspace {
        Ok(path) => out.push_str(&format!("Workspace:          {}\n", path.display())),
        Err(err) => out.push_str(&format!("Workspace:          error — {}\n", err)),
    }

    out.push_str("\nAPI keys:\n");
    out.push_str(&format!(
        "  OpenRouter:       {}\n",
        if summary.openrouter_key_present {
            "present"
        } else {
            "missing"
        }
    ));
    out.push_str(&format!(
        "  Opencode:         {}\n",
        if summary.opencode_key_present {
            "present"
        } else {
            "missing"
        }
    ));

    out.push_str("\nTelegram:\n");
    out.push_str(&format!(
        "  Bot token:        {}\n",
        if summary.token_present {
            "present"
        } else {
            "missing"
        }
    ));
    let dm_access = match (&summary.allowed_dm_username, summary.bound_dm_user_id) {
        (None, _) => "open (no restriction)".to_string(),
        (Some(username), None) => format!(
            "restricted to @{} (unbound — first matching message will bind)",
            username
        ),
        (Some(username), Some(_)) => format!("restricted to @{} (bound)", username),
    };
    out.push_str(&format!("  DM access:        {}\n", dm_access));

    out.push_str("\nOptions:\n");
    out.push_str(&format!(
        "  Tool authoring:   {}\n",
        if summary.tool_authoring { "on" } else { "off" }
    ));
    out.push_str(&format!("  Max steps:        {}\n", summary.max_steps));
    out.push_str(&format!("  Max retries:      {}\n", summary.max_retries));
    out.push_str(&format!("  Timeout:          {}s\n", summary.timeout_secs));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::TOPAGENT_WORKSPACE_KEY;
    use crate::config::defaults::TelegramModeDefaults;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_render_contract_summary_shows_fields_without_secret_values() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            (
                "OPENROUTER_API_KEY".to_string(),
                "sk-real-secret".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "123456:token-secret".to_string(),
            ),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::defaults::TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "operator".to_string(),
            ),
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
        let summary = resolve_contract_summary(&params, &defaults);
        let output = render_contract_summary(&summary);

        assert!(output.contains("Provider:"), "must show provider: {output}");
        assert!(output.contains("Model:"), "must show model: {output}");
        assert!(output.contains("Workspace:"), "must show workspace: {output}");
        assert!(output.contains("OpenRouter:"), "must show OpenRouter key status: {output}");
        assert!(output.contains("Bot token:"), "must show token status: {output}");
        assert!(output.contains("DM access:"), "must show DM access: {output}");
        assert!(output.contains("present"), "must indicate key present: {output}");
        assert!(!output.contains("sk-real-secret"), "must not reveal OpenRouter key: {output}");
        assert!(!output.contains("token-secret"), "must not reveal Telegram token: {output}");
        assert!(output.contains("operator"), "username is safe to show: {output}");
    }

    #[test]
    fn test_render_contract_summary_shows_override_and_default_when_different() {
        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::defaults::TOPAGENT_MODEL_KEY.to_string(),
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
        let output = render_contract_summary(&summary);

        assert!(output.contains("override/model"), "must show effective (overridden) model: {output}");
        assert!(output.contains("persisted/model"), "must show configured default model: {output}");
        assert!(output.contains("CLI override"), "must label the source: {output}");
    }

    #[test]
    fn test_render_contract_summary_dm_access_shows_admission_state() {
        let workspace = TempDir::new().unwrap();

        let values_unbound = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            (
                TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                crate::config::defaults::TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
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
        let output_unbound =
            render_contract_summary(&resolve_contract_summary(&params, &defaults_unbound));
        assert!(
            output_unbound.contains("unbound"),
            "must say unbound before first message: {output_unbound}"
        );

        let mut values_bound = values_unbound.clone();
        values_bound.insert(
            crate::config::defaults::TELEGRAM_BOUND_DM_USER_ID_KEY.to_string(),
            "424242".to_string(),
        );
        let defaults_bound = TelegramModeDefaults::from_metadata(&values_bound);
        let output_bound =
            render_contract_summary(&resolve_contract_summary(&params, &defaults_bound));
        assert!(output_bound.contains("bound"), "must say bound after first message: {output_bound}");
        assert!(!output_bound.contains("424242"), "must not reveal numeric bound ID: {output_bound}");
    }
}

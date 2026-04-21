use anyhow::{Context, Result};
use dialoguer::Select;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use crate::config::defaults::{
    CliParams, OPENCODE_API_KEY_KEY, OPENROUTER_API_KEY_KEY, TELEGRAM_BOT_TOKEN_KEY,
    TOPAGENT_WORKSPACE_KEY, TelegramModeDefaults,
};
use crate::config::keys::{
    require_opencode_api_key, require_openrouter_api_key, require_telegram_token,
};
use crate::config::model_selection::{
    SelectedProvider, build_route_from_resolved, canonicalize_allowed_username,
    current_configured_model, resolve_model_choice,
};
use crate::config::runtime::{
    TelegramModeConfig, build_runtime_options_with_defaults, resolve_telegram_mode_config,
};
use crate::commands::surface::PRODUCT_NAME;
use crate::managed_files::{assert_managed_or_absent, read_managed_env_metadata};
use crate::openrouter_models::{
    OpenRouterCatalogSource, discover_install_openrouter_models, humanize_age,
    openrouter_model_cache_path,
};
use crate::operational_paths::service_paths;

use super::detect::detect_install_root;
use super::lifecycle::{ServiceConfigApplyAction, install_service_with_config};
use super::managed_env::trim_nonempty;
use super::systemd::ensure_systemd_user_available;

const CUSTOM_MODEL_OPTION_LABEL: &str = "Custom model ID (type manually)";

#[derive(Debug, Clone)]
struct InstallModelPrompt {
    models: Vec<String>,
    default_model: String,
    source: OpenRouterCatalogSource,
    live_error: Option<String>,
    provider: SelectedProvider,
}

pub(crate) fn run_install(params: CliParams) -> Result<()> {
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;
    let existing_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let defaults = TelegramModeDefaults::from_metadata(&existing_values);
    let workspace = resolve_install_workspace_path(params.workspace, &existing_values)?;

    println!("{PRODUCT_NAME} setup");
    println!("This will configure and start your Telegram background service.");
    println!();

    let selected_provider = prompt_for_install_provider(defaults.provider)?;

    let (api_key, opencode_api_key) = match selected_provider {
        SelectedProvider::OpenRouter => {
            let key = prompt_for_install_value(
                "OpenRouter API key",
                params.api_key.as_deref().or_else(|| {
                    existing_values
                        .get(OPENROUTER_API_KEY_KEY)
                        .map(String::as_str)
                }),
                require_openrouter_api_key,
            )?;
            let opencode_key = prompt_for_install_value_optional(
                "Opencode API key (optional, press Enter to skip)",
                params.opencode_api_key.as_deref().or_else(|| {
                    existing_values
                        .get(OPENCODE_API_KEY_KEY)
                        .map(String::as_str)
                }),
            );
            (Some(key), opencode_key)
        }
        SelectedProvider::Opencode => {
            let key = prompt_for_install_value(
                "Opencode API key",
                params.opencode_api_key.as_deref().or_else(|| {
                    existing_values
                        .get(OPENCODE_API_KEY_KEY)
                        .map(String::as_str)
                }),
                require_opencode_api_key,
            )?;
            (None, Some(key))
        }
    };

    let explicit_model = trim_nonempty(params.model.clone());
    let provider_kind = selected_provider.to_provider_kind();
    let selected_model = if explicit_model.is_some() {
        let resolved = resolve_model_choice(
            provider_kind,
            params.model.clone(),
            None,
            defaults.model.clone(),
        );
        println!(
            "{} model: {} (--model)",
            selected_provider.label(),
            resolved.model_id
        );
        None
    } else {
        Some(prompt_for_install_model(
            selected_provider,
            api_key.as_deref().or(opencode_api_key.as_deref()),
            defaults.model.clone(),
        )?)
    };

    let token = prompt_for_install_value(
        "Telegram bot token",
        existing_values
            .get(TELEGRAM_BOT_TOKEN_KEY)
            .map(String::as_str),
        require_telegram_token,
    )?;

    let allowed_username = prompt_for_install_username(defaults.allowed_dm_username.as_deref())?;

    let resolved_model = resolve_model_choice(
        provider_kind,
        params.model,
        selected_model,
        defaults.model.clone(),
    );
    // Preserve the bound DM user id when the operator kept the same allowed
    // username; otherwise reset binding so Telegram rebinds against the new
    // admission policy on the next matched message.
    let preserved_bound_dm_user_id = if allowed_username == defaults.allowed_dm_username {
        defaults.bound_dm_user_id
    } else {
        None
    };
    let configured_default_model =
        resolve_model_choice(provider_kind, None, None, defaults.model.clone()).model_id;
    let config = TelegramModeConfig {
        token,
        openrouter_api_key: api_key,
        opencode_api_key,
        configured_default_model,
        route: build_route_from_resolved(&resolved_model),
        workspace,
        options: build_runtime_options_with_defaults(
            params.max_steps,
            params.max_retries,
            params.timeout_secs,
            params.generated_tool_authoring,
            &defaults,
        ),
        selected_provider,
        allowed_dm_username: allowed_username,
        bound_dm_user_id: preserved_bound_dm_user_id,
    };
    let service_action = install_service_with_config(&config, &paths)?;

    println!();
    print_service_installed(
        &format!("{PRODUCT_NAME} installed."),
        Some(&config.workspace),
        service_action,
    );

    Ok(())
}

pub(super) fn run_service_install(token: Option<String>, params: CliParams) -> Result<()> {
    let paths = service_paths()?;
    let existing_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let config = resolve_telegram_mode_config(
        token,
        params,
        TelegramModeDefaults::from_metadata(&existing_values),
    )?;
    let service_action = install_service_with_config(&config, &paths)?;
    print_service_installed(
        &format!("{PRODUCT_NAME} service installed."),
        Some(&config.workspace),
        service_action,
    );
    Ok(())
}

fn print_service_installed(
    headline: &str,
    workspace: Option<&PathBuf>,
    service_action: ServiceConfigApplyAction,
) {
    println!("{}", headline);
    if let Some(ws) = workspace {
        println!("Workspace: {}", ws.display());
    }
    println!("Service action: {}", service_action.label());
    println!();
    println!("Open a private chat with your bot and send a message to start.");
    println!("Run `topagent status` to check service health.");
}

fn resolve_install_workspace_path(
    workspace: Option<PathBuf>,
    existing_values: &HashMap<String, String>,
) -> Result<PathBuf> {
    let target = if let Some(workspace) = workspace {
        workspace
    } else if let Some(existing_workspace) = existing_values.get(TOPAGENT_WORKSPACE_KEY) {
        PathBuf::from(existing_workspace)
    } else {
        detect_install_root()?.join("workspace")
    };
    ensure_directory(target)
}

fn ensure_directory(path: PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    path.canonicalize()
        .with_context(|| format!("failed to access {}", path.display()))
}

fn prompt_for_install_value(
    label: &str,
    existing_value: Option<&str>,
    validator: fn(Option<String>) -> Result<String>,
) -> Result<String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();

    loop {
        if existing_value.is_some() {
            print!("{label} [press Enter to keep the current value]: ");
        } else {
            print!("{label}: ");
        }
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .context("failed to read installer input")?;
        if read == 0 {
            return Err(anyhow::anyhow!(
                "Installer input ended unexpectedly. Re-run `topagent install` in an interactive shell."
            ));
        }

        let candidate = line.trim();
        let value = if candidate.is_empty() {
            existing_value.map(str::to_string)
        } else {
            Some(candidate.to_string())
        };

        match validator(value) {
            Ok(value) => return Ok(value),
            Err(err) => {
                println!("{}", err);
            }
        }
    }
}

fn prompt_for_install_value_optional(label: &str, existing_value: Option<&str>) -> Option<String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();

    if existing_value.is_some() {
        print!("{label} [press Enter to keep the current value, or type 'clear' to remove]: ");
    } else {
        print!("{label}: ");
    }
    io::stdout().flush().ok()?;

    let mut line = String::new();
    let read = input.read_line(&mut line).ok()?;
    if read == 0 {
        return None;
    }

    let candidate = line.trim();
    if candidate.eq_ignore_ascii_case("clear") {
        None
    } else if candidate.is_empty() {
        existing_value.map(str::to_string)
    } else {
        Some(candidate.to_string())
    }
}

pub(super) fn prompt_for_install_model(
    provider: SelectedProvider,
    api_key: Option<&str>,
    existing_model: Option<String>,
) -> Result<String> {
    let prompt = build_install_model_prompt(provider, api_key, existing_model)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout();
    prompt_for_install_model_with_io(&mut input, &mut output, &prompt)
}

fn build_install_model_prompt(
    provider: SelectedProvider,
    api_key: Option<&str>,
    existing_model: Option<String>,
) -> Result<InstallModelPrompt> {
    let default_model =
        current_configured_model(provider.to_provider_kind(), existing_model.clone()).model_id;

    match provider {
        SelectedProvider::OpenRouter => {
            let cache_path = openrouter_model_cache_path()?;
            let discovered = discover_install_openrouter_models(&cache_path, api_key)?;
            Ok(InstallModelPrompt {
                models: discovered.models,
                default_model,
                source: discovered.source,
                live_error: discovered.live_error,
                provider,
            })
        }
        SelectedProvider::Opencode => Ok(InstallModelPrompt {
            models: vec![
                "glm-4".to_string(),
                "glm-4-flash".to_string(),
                "glm-3".to_string(),
                "kimi-k2".to_string(),
                "qwen/qwen3.6-plus".to_string(),
            ],
            default_model: if existing_model
                .as_ref()
                .map(|m| {
                    // Keep the existing model as picker default if it looks like an
                    // Opencode model (cosmetic UI heuristic only — not used for routing).
                    let l = m.to_lowercase();
                    l.starts_with("glm-")
                        || l.starts_with("kimi-")
                        || l.starts_with("mimo-")
                        || l.starts_with("qwen/")
                        || l == "opencode"
                })
                .unwrap_or(false)
            {
                default_model
            } else {
                "glm-4".to_string()
            },
            source: OpenRouterCatalogSource::CuratedFallback,
            live_error: None,
            provider,
        }),
    }
}

fn prompt_for_install_model_with_io<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &InstallModelPrompt,
) -> Result<String> {
    let provider_label = prompt.provider.label();
    print_install_model_source(output, &prompt.source, prompt.live_error.as_deref())?;
    writeln!(output, "{} model:", provider_label).context("failed to write installer output")?;
    for (index, model) in prompt.models.iter().enumerate() {
        let marker = if *model == prompt.default_model {
            " [default]"
        } else {
            ""
        };
        writeln!(output, "  {}. {}{}", index + 1, model, marker)
            .context("failed to write installer output")?;
    }
    writeln!(
        output,
        "  {}. {}",
        prompt.models.len() + 1,
        CUSTOM_MODEL_OPTION_LABEL
    )
    .context("failed to write installer output")?;

    loop {
        write!(
            output,
            "Select {} model [press Enter to keep {}]: ",
            provider_label, prompt.default_model
        )
        .context("failed to write installer output")?;
        output.flush().context("failed to flush stdout")?;

        let line = read_install_input_line(input)?;
        let candidate = line.trim();
        if candidate.is_empty() {
            return Ok(prompt.default_model.clone());
        }

        let Ok(choice) = candidate.parse::<usize>() else {
            writeln!(output, "Enter a number from the menu above.")
                .context("failed to write installer output")?;
            continue;
        };

        if (1..=prompt.models.len()).contains(&choice) {
            return Ok(prompt.models[choice - 1].clone());
        }
        if choice == prompt.models.len() + 1 {
            return prompt_for_custom_model_with_io(input, output, provider_label);
        }

        writeln!(output, "Enter a number from the menu above.")
            .context("failed to write installer output")?;
    }
}

fn prompt_for_custom_model_with_io<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    provider_label: &str,
) -> Result<String> {
    loop {
        write!(output, "Custom {} model ID: ", provider_label)
            .context("failed to write installer output")?;
        output.flush().context("failed to flush stdout")?;

        let line = read_install_input_line(input)?;
        let candidate = line.trim();
        if candidate.is_empty() {
            writeln!(output, "Model ID cannot be empty.")
                .context("failed to write installer output")?;
            continue;
        }
        return Ok(candidate.to_string());
    }
}

fn print_install_model_source<W: Write>(
    output: &mut W,
    source: &OpenRouterCatalogSource,
    live_error: Option<&str>,
) -> Result<()> {
    match source {
        OpenRouterCatalogSource::Live => {
            writeln!(output, "Fetched current top OpenRouter models.")
                .context("failed to write installer output")?;
        }
        OpenRouterCatalogSource::Cache { age_secs } => {
            if let Some(err) = live_error {
                writeln!(
                    output,
                    "Live OpenRouter model lookup failed ({}). Using cached models from {} ago.",
                    err,
                    humanize_age(*age_secs)
                )
                .context("failed to write installer output")?;
            } else {
                writeln!(
                    output,
                    "Using cached OpenRouter models from {} ago.",
                    humanize_age(*age_secs)
                )
                .context("failed to write installer output")?;
            }
        }
        OpenRouterCatalogSource::CuratedFallback => {
            if let Some(err) = live_error {
                writeln!(
                    output,
                    "Live OpenRouter model lookup failed ({}). Using a starter model list.",
                    err
                )
                .context("failed to write installer output")?;
            } else {
                writeln!(output, "Using a starter OpenRouter model list.")
                    .context("failed to write installer output")?;
            }
        }
    }
    Ok(())
}

fn read_install_input_line<R: BufRead>(input: &mut R) -> Result<String> {
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .context("failed to read installer input")?;
    if read == 0 {
        return Err(anyhow::anyhow!(
            "Installer input ended unexpectedly. Re-run `topagent install` in an interactive shell."
        ));
    }
    Ok(line)
}

fn prompt_for_install_provider(
    existing_provider: Option<SelectedProvider>,
) -> Result<SelectedProvider> {
    let providers = [SelectedProvider::OpenRouter, SelectedProvider::Opencode];
    let labels: Vec<&str> = providers.iter().map(|p| p.label()).collect();
    let default_index = existing_provider
        .and_then(|ep| providers.iter().position(|p| *p == ep))
        .unwrap_or(0);

    let selection = Select::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Provider")
        .items(&labels)
        .default(default_index)
        .interact()
        .context("provider selection requires an interactive terminal; re-run `topagent install` in a terminal")?;

    Ok(providers[selection])
}

fn prompt_for_install_username(existing_username: Option<&str>) -> Result<Option<String>> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout();
    prompt_for_install_username_with_io(&mut input, &mut output, existing_username)
}

fn prompt_for_install_username_with_io<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    existing_username: Option<&str>,
) -> Result<Option<String>> {
    loop {
        if existing_username.is_some() {
            write!(
                output,
                "Allowed Telegram username (optional, press Enter to keep current): "
            )
            .context("failed to write installer output")?;
        } else {
            write!(
                output,
                "Allowed Telegram username for direct messages (optional, press Enter to skip): "
            )
            .context("failed to write installer output")?;
        }
        output.flush().context("failed to flush output")?;

        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .context("failed to read installer input")?;
        if read == 0 {
            return Ok(None);
        }

        let candidate = line.trim();
        if candidate.is_empty() {
            return Ok(existing_username.and_then(canonicalize_allowed_username));
        }

        let Some(normalized) = canonicalize_allowed_username(candidate) else {
            writeln!(output, "Username cannot be empty.")
                .context("failed to write installer output")?;
            continue;
        };

        if normalized.contains(' ') {
            writeln!(output, "Username cannot contain spaces.")
                .context("failed to write installer output")?;
            continue;
        }

        return Ok(Some(normalized));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_prompt_for_install_username_strips_leading_at_and_lowercases() {
        // With @ prefix
        let mut input = Cursor::new("@MyUser\n");
        let mut output = Vec::new();
        let result = prompt_for_install_username_with_io(&mut input, &mut output, None).unwrap();
        assert_eq!(result, Some("myuser".to_string()));

        // Without @ prefix
        let mut input2 = Cursor::new("MyUser\n");
        let mut output2 = Vec::new();
        let result2 = prompt_for_install_username_with_io(&mut input2, &mut output2, None).unwrap();
        assert_eq!(result2, Some("myuser".to_string()));

        // Multiple @ prefixes
        let mut input3 = Cursor::new("@@MyUser\n");
        let mut output3 = Vec::new();
        let result3 = prompt_for_install_username_with_io(&mut input3, &mut output3, None).unwrap();
        assert_eq!(result3, Some("myuser".to_string()));
    }

    #[test]
    fn test_prompt_for_install_username_rejects_only_at_sign() {
        let mut input = Cursor::new("@\n");
        let mut output = Vec::new();
        let result = prompt_for_install_username_with_io(&mut input, &mut output, None).unwrap();
        // "@" alone normalizes to empty, loop should re-prompt; with only one line of
        // input the function will hit EOF and return Ok(None)
        assert_eq!(result, None);
    }

    #[test]
    fn test_prompt_for_install_username_keeps_existing_on_empty_input() {
        let mut input = Cursor::new("\n");
        let mut output = Vec::new();
        let result =
            prompt_for_install_username_with_io(&mut input, &mut output, Some("existinguser"))
                .unwrap();
        assert_eq!(result, Some("existinguser".to_string()));
    }

    #[test]
    fn test_prompt_for_install_model_custom_entry_path_rejects_empty_input() {
        let prompt = InstallModelPrompt {
            models: vec![
                "minimax/minimax-m2.7".to_string(),
                "qwen/qwen3.6-plus".to_string(),
            ],
            default_model: "minimax/minimax-m2.7".to_string(),
            source: OpenRouterCatalogSource::CuratedFallback,
            live_error: Some("timeout".to_string()),
            provider: SelectedProvider::OpenRouter,
        };
        let mut input = Cursor::new("3\n\ncustom/model\n");
        let mut output = Vec::new();

        let selected = prompt_for_install_model_with_io(&mut input, &mut output, &prompt).unwrap();
        let rendered = String::from_utf8(output).unwrap();

        assert_eq!(selected, "custom/model");
        assert!(rendered.contains("Using a starter model list"));
        assert!(rendered.contains(CUSTOM_MODEL_OPTION_LABEL));
        assert!(rendered.contains("Model ID cannot be empty."));
    }

    #[test]
    fn test_prompt_for_install_model_enter_keeps_default_model() {
        let prompt = InstallModelPrompt {
            models: vec!["qwen/qwen3.6-plus".to_string()],
            default_model: "persisted/model".to_string(),
            source: OpenRouterCatalogSource::Cache { age_secs: 75 },
            live_error: Some("network down".to_string()),
            provider: SelectedProvider::OpenRouter,
        };
        let mut input = Cursor::new("\n");
        let mut output = Vec::new();

        let selected = prompt_for_install_model_with_io(&mut input, &mut output, &prompt).unwrap();
        let rendered = String::from_utf8(output).unwrap();

        assert_eq!(selected, "persisted/model");
        assert!(rendered.contains("Using cached models from 1m ago"));
    }
}

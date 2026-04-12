use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use crate::config::*;
use crate::managed_files::{assert_managed_or_absent, read_managed_env_metadata};
use crate::openrouter_models::{
    discover_install_openrouter_models, humanize_age, openrouter_model_cache_path,
    OpenRouterCatalogSource,
};
use crate::operational_paths::service_paths;

use super::lifecycle::{
    detect_install_root, ensure_systemd_user_available, install_service_with_config,
    ServiceConfigApplyAction,
};
use super::managed_env::{trim_nonempty, OPENROUTER_API_KEY_KEY, TELEGRAM_BOT_TOKEN_KEY};

const CUSTOM_MODEL_OPTION_LABEL: &str = "Custom model ID (type manually)";

#[derive(Debug, Clone)]
struct InstallModelPrompt {
    models: Vec<String>,
    default_model: String,
    source: OpenRouterCatalogSource,
    live_error: Option<String>,
}

pub(crate) fn run_install(params: CliParams) -> Result<()> {
    ensure_systemd_user_available()?;
    let paths = service_paths()?;
    assert_managed_or_absent(&paths.unit_path, "service unit")?;
    assert_managed_or_absent(&paths.env_path, "service env file")?;
    let existing_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let defaults = TelegramModeDefaults::from_metadata(&existing_values);
    let workspace = resolve_install_workspace_path(params.workspace, &existing_values)?;

    println!("TopAgent setup");
    println!("This will configure and start your Telegram background service.");
    println!();

    let api_key = prompt_for_install_value(
        "OpenRouter API key",
        params.api_key.as_deref().or_else(|| {
            existing_values
                .get(OPENROUTER_API_KEY_KEY)
                .map(String::as_str)
        }),
        require_openrouter_api_key,
    )?;
    let explicit_model = trim_nonempty(params.model.clone());
    let selected_model = if explicit_model.is_some() {
        let resolved = resolve_model_choice(params.model.clone(), None, defaults.model.clone());
        println!("OpenRouter model: {} (--model)", resolved.model_id);
        None
    } else {
        Some(prompt_for_install_model(
            Some(api_key.as_str()),
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

    let resolved_model = resolve_model_choice(params.model, selected_model, defaults.model.clone());
    let config = TelegramModeConfig {
        token,
        api_key,
        route: build_route_from_resolved(&resolved_model),
        workspace,
        options: build_runtime_options_with_defaults(
            params.max_steps,
            params.max_retries,
            params.timeout_secs,
            params.generated_tool_authoring,
            &defaults,
        ),
    };
    let service_action = install_service_with_config(&config, &paths)?;

    println!();
    print_service_installed(
        "TopAgent installed.",
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
        "TopAgent service installed.",
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

pub(super) fn prompt_for_install_model(
    api_key: Option<&str>,
    existing_model: Option<String>,
) -> Result<String> {
    let prompt = build_install_model_prompt(api_key, existing_model)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout();
    prompt_for_install_model_with_io(&mut input, &mut output, &prompt)
}

fn build_install_model_prompt(
    api_key: Option<&str>,
    existing_model: Option<String>,
) -> Result<InstallModelPrompt> {
    let cache_path = openrouter_model_cache_path()?;
    let discovered = discover_install_openrouter_models(&cache_path, api_key)?;
    let default_model = current_configured_model(existing_model).model_id;
    Ok(InstallModelPrompt {
        models: discovered.models,
        default_model,
        source: discovered.source,
        live_error: discovered.live_error,
    })
}

fn prompt_for_install_model_with_io<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &InstallModelPrompt,
) -> Result<String> {
    print_install_model_source(output, &prompt.source, prompt.live_error.as_deref())?;
    writeln!(output, "OpenRouter model:").context("failed to write installer output")?;
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
            "Select OpenRouter model [press Enter to keep {}]: ",
            prompt.default_model
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
            return prompt_for_custom_model_with_io(input, output);
        }

        writeln!(output, "Enter a number from the menu above.")
            .context("failed to write installer output")?;
    }
}

fn prompt_for_custom_model_with_io<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<String> {
    loop {
        write!(output, "Custom OpenRouter model ID: ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
        };
        let mut input = Cursor::new("\n");
        let mut output = Vec::new();

        let selected = prompt_for_install_model_with_io(&mut input, &mut output, &prompt).unwrap();
        let rendered = String::from_utf8(output).unwrap();

        assert_eq!(selected, "persisted/model");
        assert!(rendered.contains("Using cached models from 1m ago"));
    }
}

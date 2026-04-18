use anyhow::Result;
use std::path::PathBuf;

use crate::config::{SelectedProvider, *};
use crate::managed_files::{assert_managed_or_absent, read_managed_env_metadata};
use crate::openrouter_models::{
    fetch_openrouter_top_models, humanize_age, load_cached_openrouter_models,
    openrouter_model_cache_path, save_cached_openrouter_models,
};
use crate::operational_paths::{service_paths, ServicePaths};
use topagent_core::ProviderKind;

use super::install::prompt_for_install_model;
use super::lifecycle::restart_service_if_installed;
use super::managed_env::{
    persisted_model_from_env_values, trim_nonempty, write_managed_env_values,
};
use super::state::load_control_plane_state;

#[derive(Debug, Clone)]
struct ModelUpdateReport {
    previous_model: String,
    configured_model: String,
    selection_source: Option<ModelResolutionSource>,
    service_restarted: bool,
    config_path: PathBuf,
}

pub(crate) fn run_model_command(command: crate::ModelCommands, params: CliParams) -> Result<()> {
    match command {
        crate::ModelCommands::Status => run_model_status(params),
        crate::ModelCommands::Set { model_id } => run_model_set(model_id),
        crate::ModelCommands::Pick => run_model_pick(params),
        crate::ModelCommands::List => run_model_list(),
        crate::ModelCommands::Refresh => run_model_refresh(params),
    }
}

fn run_model_status(params: CliParams) -> Result<()> {
    let state = load_control_plane_state(params.model)?;
    let cache_path = openrouter_model_cache_path()?;
    let cached = load_cached_openrouter_models(&cache_path)?;
    let default_provider = resolve_provider_for_model(&state.model_selection.effective.model_id);

    println!("TopAgent model status");
    println!(
        "Configured default model: {} [{}] ({})",
        state.model_selection.configured_default.model_id,
        resolve_provider_for_model(&state.model_selection.configured_default.model_id),
        state.model_selection.configured_default.source.label()
    );
    println!(
        "Effective model: {} [{}] ({})",
        state.model_selection.effective.model_id,
        default_provider,
        state.model_selection.effective.source.label()
    );
    println!(
        "Setup installed: {}",
        if state.setup_installed { "yes" } else { "no" }
    );
    println!(
        "Service installed: {}",
        if state.service_probe.service_installed {
            "yes"
        } else {
            "no"
        }
    );
    println!("Config file: {}", state.paths.env_path.display());

    if let Some(cached) = cached {
        println!(
            "Cached models: {} (updated {} ago)",
            cached.models.len(),
            humanize_age(cached.age_secs)
        );
    } else {
        println!("Cached models: none");
        println!("Hint: run `topagent model refresh` to fetch current top models.");
    }

    Ok(())
}

fn run_model_set(model_id: String) -> Result<()> {
    let model_id = model_id.trim().to_string();
    if model_id.is_empty() {
        return Err(anyhow::anyhow!("Model ID cannot be empty."));
    }

    let provider = resolve_provider_for_model(&model_id);
    let paths = service_paths()?;
    let report = update_configured_model(&paths, model_id, None)?;

    println!("TopAgent model updated.");
    println!("Previous model: {}", report.previous_model);
    println!(
        "Configured model: {} [{}]",
        report.configured_model, provider
    );
    println!("Config file: {}", report.config_path.display());
    println!(
        "Service restart: {}",
        if report.service_restarted {
            "yes"
        } else {
            "not needed (service not installed)"
        }
    );

    Ok(())
}

fn run_model_pick(params: CliParams) -> Result<()> {
    let paths = service_paths()?;
    if !paths.env_path.exists() {
        return Err(anyhow::anyhow!(
            "TopAgent is not set up yet. Run `topagent setup` first."
        ));
    }
    assert_managed_or_absent(&paths.env_path, "service env file")?;

    let values = read_managed_env_metadata(&paths.env_path)?;
    let api_key = trim_nonempty(params.api_key)
        .or_else(|| trim_nonempty(std::env::var(OPENROUTER_API_KEY_KEY).ok()))
        .or_else(|| {
            values
                .get(OPENROUTER_API_KEY_KEY)
                .map(String::to_string)
                .and_then(|value| trim_nonempty(Some(value)))
        });

    let explicit_model = trim_nonempty(params.model.clone());
    let resolved_for_check = resolve_model_choice(
        explicit_model.clone(),
        None,
        persisted_model_from_env_values(&values),
    );
    let provider = match resolve_provider_for_model(&resolved_for_check.model_id) {
        ProviderKind::OpenRouter => SelectedProvider::OpenRouter,
        ProviderKind::Opencode => SelectedProvider::Opencode,
    };
    let selected_model = if explicit_model.is_some() {
        let resolved = resolve_model_choice(
            params.model.clone(),
            None,
            persisted_model_from_env_values(&values),
        );
        println!("Model: {} (--model)", resolved.model_id);
        None
    } else {
        Some(prompt_for_install_model(
            provider,
            api_key.as_deref(),
            persisted_model_from_env_values(&values),
        )?)
    };

    let resolved_model = resolve_model_choice(
        params.model,
        selected_model,
        persisted_model_from_env_values(&values),
    );
    let report = update_configured_model(
        &paths,
        resolved_model.model_id.clone(),
        Some(resolved_model.source),
    )?;

    println!("TopAgent model updated.");
    println!("Previous model: {}", report.previous_model);
    println!(
        "Configured model: {} [{}]",
        report.configured_model,
        provider.label()
    );
    println!(
        "Selection source: {}",
        report
            .selection_source
            .expect("interactive model selection should record a source")
            .label()
    );
    println!("Config file: {}", report.config_path.display());
    println!(
        "Service restart: {}",
        if report.service_restarted {
            "yes"
        } else {
            "not needed (service not installed)"
        }
    );

    Ok(())
}

fn run_model_list() -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let cache_path = openrouter_model_cache_path()?;
    let current_model =
        current_configured_model(persisted_model_from_env_values(&env_values)).model_id;
    let current_provider = resolve_provider_for_model(&current_model);
    let Some(cached) = load_cached_openrouter_models(&cache_path)? else {
        println!("TopAgent model list");
        println!("Current model: {} [{}]", current_model, current_provider);
        println!("No cached OpenRouter model list found.");
        println!("Run `topagent model refresh` to fetch the current top models.");
        return Ok(());
    };

    println!("TopAgent model list");
    println!(
        "Cached top models: {} (updated {} ago)",
        cached.models.len(),
        humanize_age(cached.age_secs)
    );
    for model in &cached.models {
        let marker = if *model == current_model {
            " (current)"
        } else {
            ""
        };
        println!("  {}{}", model, marker);
    }
    if !cached.models.iter().any(|model| model == &current_model) {
        println!("Current model: {} (not in cached list)", current_model);
    }

    Ok(())
}

fn run_model_refresh(params: CliParams) -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let cache_path = openrouter_model_cache_path()?;
    let api_key = trim_nonempty(params.api_key)
        .or_else(|| trim_nonempty(std::env::var(OPENROUTER_API_KEY_KEY).ok()))
        .or_else(|| {
            env_values
                .get(OPENROUTER_API_KEY_KEY)
                .map(String::to_string)
                .and_then(|value| trim_nonempty(Some(value)))
        });

    match fetch_openrouter_top_models(api_key.as_deref()) {
        Ok(models) => {
            save_cached_openrouter_models(&cache_path, &models)?;
            println!("Refreshed OpenRouter model cache.");
            println!("Models cached: {}", models.len());
            println!("Cache file: {}", cache_path.display());
            Ok(())
        }
        Err(err) => {
            if let Some(cached) = load_cached_openrouter_models(&cache_path)? {
                println!(
                    "Live OpenRouter model refresh failed ({}). Keeping cached models from {} ago.",
                    err,
                    humanize_age(cached.age_secs)
                );
                println!("Cache file: {}", cache_path.display());
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Failed to refresh the OpenRouter model cache. {}",
                    err
                ))
            }
        }
    }
}

fn update_configured_model(
    paths: &ServicePaths,
    model_id: String,
    selection_source: Option<ModelResolutionSource>,
) -> Result<ModelUpdateReport> {
    if !paths.env_path.exists() {
        return Err(anyhow::anyhow!(
            "TopAgent is not installed yet. Run `topagent install` first."
        ));
    }
    assert_managed_or_absent(&paths.env_path, "service env file")?;

    let mut values = read_managed_env_metadata(&paths.env_path)?;
    let previous_model =
        current_configured_model(persisted_model_from_env_values(&values)).model_id;
    values.insert(TOPAGENT_MODEL_KEY.to_string(), model_id.clone());
    write_managed_env_values(&paths.env_path, &values)?;

    let service_restarted = restart_service_if_installed(paths).map_err(|err| {
        anyhow::anyhow!(
            "Updated the configured model from {} to {} in {}, but failed to restart the TopAgent Telegram service. {}",
            previous_model,
            model_id,
            paths.env_path.display(),
            err
        )
    })?;

    Ok(ModelUpdateReport {
        previous_model,
        configured_model: model_id,
        selection_source,
        service_restarted,
        config_path: paths.env_path.clone(),
    })
}

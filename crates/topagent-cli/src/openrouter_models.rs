use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::OffsetDateTime;

use crate::config::resolve_config_home;

const OPENROUTER_MODELS_ENDPOINT: &str = "https://openrouter.ai/api/v1/models";
const OPENROUTER_RANKINGS_URL: &str = "https://openrouter.ai/rankings";
const OPENROUTER_CACHE_DIR: &str = "topagent/cache";
const OPENROUTER_CACHE_FILE: &str = "openrouter-models.json";
pub(crate) const OPENROUTER_ONBOARDING_MODEL_LIMIT: usize = 8;
const RANKINGS_SECTION_MARKER: &str = "grid grid-cols-12 items-center";
pub(crate) const CURATED_OPENROUTER_MODELS: &[&str] = &[
    "minimax/minimax-m2.7",
    "qwen/qwen3.6-plus",
    "anthropic/claude-sonnet-4.6",
    "openai/gpt-5.4-mini",
    "google/gemini-3.1-pro-preview",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenRouterCatalogSource {
    Live,
    Cache { age_secs: u64 },
    CuratedFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredOpenRouterModels {
    pub(crate) models: Vec<String>,
    pub(crate) source: OpenRouterCatalogSource,
    pub(crate) live_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedOpenRouterModels {
    pub(crate) models: Vec<String>,
    pub(crate) age_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OpenRouterModelCache {
    updated_at_unix_secs: i64,
    models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenRouterModelSummary {
    id: String,
    canonical_slug: String,
}

pub(crate) fn openrouter_model_cache_path() -> Result<PathBuf> {
    Ok(resolve_config_home()?
        .join(OPENROUTER_CACHE_DIR)
        .join(OPENROUTER_CACHE_FILE))
}

pub(crate) fn curated_openrouter_models() -> Vec<String> {
    CURATED_OPENROUTER_MODELS
        .iter()
        .map(|model| (*model).to_string())
        .collect()
}

pub(crate) fn load_cached_openrouter_models(path: &Path) -> Result<Option<CachedOpenRouterModels>> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read(path).with_context(|| {
        format!(
            "failed to read OpenRouter model cache at {}",
            path.display()
        )
    })?;
    let cache: OpenRouterModelCache =
        serde_json::from_slice(&contents).context("failed to parse OpenRouter model cache")?;
    let models = normalize_model_ids(cache.models);
    if models.is_empty() {
        return Ok(None);
    }

    Ok(Some(CachedOpenRouterModels {
        age_secs: model_cache_age_secs(cache.updated_at_unix_secs),
        models,
    }))
}

pub(crate) fn save_cached_openrouter_models(path: &Path, models: &[String]) -> Result<()> {
    let normalized = normalize_model_ids(models.to_vec());
    if normalized.is_empty() {
        bail!("refusing to write an empty OpenRouter model cache");
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let payload = OpenRouterModelCache {
        updated_at_unix_secs: now_unix_secs(),
        models: normalized,
    };
    let json = serde_json::to_vec_pretty(&payload)
        .context("failed to serialize OpenRouter model cache")?;
    std::fs::write(path, json).with_context(|| {
        format!(
            "failed to write OpenRouter model cache at {}",
            path.display()
        )
    })
}

pub(crate) fn discover_install_openrouter_models(
    cache_path: &Path,
    api_key: Option<&str>,
) -> Result<DiscoveredOpenRouterModels> {
    discover_install_openrouter_models_with_fetcher(
        cache_path,
        api_key,
        fetch_openrouter_top_models,
    )
}

pub(crate) fn discover_install_openrouter_models_with_fetcher<F>(
    cache_path: &Path,
    api_key: Option<&str>,
    fetcher: F,
) -> Result<DiscoveredOpenRouterModels>
where
    F: FnOnce(Option<&str>) -> Result<Vec<String>>,
{
    match fetcher(api_key) {
        Ok(models) => {
            save_cached_openrouter_models(cache_path, &models)?;
            Ok(DiscoveredOpenRouterModels {
                models: normalize_model_ids(models),
                source: OpenRouterCatalogSource::Live,
                live_error: None,
            })
        }
        Err(err) => {
            let error_text = err.to_string();
            if let Some(cache) = load_cached_openrouter_models(cache_path)? {
                return Ok(DiscoveredOpenRouterModels {
                    models: cache.models,
                    source: OpenRouterCatalogSource::Cache {
                        age_secs: cache.age_secs,
                    },
                    live_error: Some(error_text),
                });
            }

            Ok(DiscoveredOpenRouterModels {
                models: curated_openrouter_models(),
                source: OpenRouterCatalogSource::CuratedFallback,
                live_error: Some(error_text),
            })
        }
    }
}

pub(crate) fn fetch_openrouter_top_models(api_key: Option<&str>) -> Result<Vec<String>> {
    if std::env::var("TOPAGENT_DISABLE_OPENROUTER_MODEL_FETCH")
        .ok()
        .as_deref()
        == Some("1")
    {
        bail!("live OpenRouter model fetch disabled");
    }

    fn fetch_once(api_key: Option<&str>) -> Result<Vec<String>> {
        let client = build_model_fetch_client()?;
        let mut models_request = client.get(OPENROUTER_MODELS_ENDPOINT);
        if let Some(api_key) = api_key {
            models_request = models_request.bearer_auth(api_key);
        }

        let models_payload: Value = models_request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .with_context(|| format!("model fetch failed: GET {OPENROUTER_MODELS_ENDPOINT}"))?
            .json()
            .context("failed to parse OpenRouter model list response")?;
        let catalog = parse_openrouter_model_summaries(&models_payload);
        if catalog.is_empty() {
            bail!("OpenRouter model list did not include any ids");
        }

        let rankings_html = client
            .get(OPENROUTER_RANKINGS_URL)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .with_context(|| format!("model fetch failed: GET {OPENROUTER_RANKINGS_URL}"))?
            .text()
            .context("failed to read OpenRouter rankings page")?;

        let ranked_slugs =
            parse_openrouter_ranking_slugs(&rankings_html, OPENROUTER_ONBOARDING_MODEL_LIMIT);
        if ranked_slugs.is_empty() {
            bail!("OpenRouter rankings page did not include any leaderboard models");
        }

        let ranked_ids = match_openrouter_rankings_to_model_ids(
            &ranked_slugs,
            &catalog,
            OPENROUTER_ONBOARDING_MODEL_LIMIT,
        );
        if ranked_ids.is_empty() {
            bail!("OpenRouter rankings models did not match the fetched catalog");
        }

        Ok(ranked_ids)
    }

    match fetch_once(api_key) {
        Ok(models) => Ok(models),
        Err(err) if api_key.is_some() => fetch_once(None).or(Err(err)),
        Err(err) => Err(err),
    }
}

pub(crate) fn humanize_age(age_secs: u64) -> String {
    match age_secs {
        0..=59 => format!("{age_secs}s"),
        60..=3599 => format!("{}m", age_secs / 60),
        3600..=86_399 => format!("{}h", age_secs / 3600),
        _ => format!("{}d", age_secs / 86_400),
    }
}

fn build_model_fetch_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .build()
        .context("failed to build OpenRouter model-fetch HTTP client")
}

fn now_unix_secs() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn model_cache_age_secs(updated_at_unix_secs: i64) -> u64 {
    now_unix_secs()
        .saturating_sub(updated_at_unix_secs)
        .try_into()
        .unwrap_or(0)
}

fn normalize_model_ids(ids: Vec<String>) -> Vec<String> {
    let mut unique = BTreeMap::new();
    for id in ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        unique
            .entry(trimmed.to_ascii_lowercase())
            .or_insert_with(|| trimmed.to_string());
    }
    unique.into_values().collect()
}

fn parse_openrouter_model_summaries(payload: &Value) -> Vec<OpenRouterModelSummary> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array());
    let Some(entries) = entries else {
        return Vec::new();
    };

    let mut unique = BTreeMap::new();
    for model in entries {
        let Some(id) = model.get("id").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }

        let canonical_slug = model
            .get("canonical_slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|slug| !slug.is_empty())
            .unwrap_or(id);

        unique
            .entry(id.to_ascii_lowercase())
            .or_insert_with(|| OpenRouterModelSummary {
                id: id.to_string(),
                canonical_slug: canonical_slug.to_string(),
            });
    }

    unique.into_values().collect()
}

fn parse_openrouter_ranking_slugs(html: &str, limit: usize) -> Vec<String> {
    let html = html
        .find(RANKINGS_SECTION_MARKER)
        .and_then(|start| html.get(start..))
        .unwrap_or(html);
    let link_regex =
        Regex::new(r#"href="/([^"/?#]+/[^"?#]+)""#).expect("valid OpenRouter rankings link regex");

    let mut slugs = Vec::new();
    for capture in link_regex.captures_iter(html) {
        let Some(slug) = capture.get(1).map(|value| value.as_str().trim()) else {
            continue;
        };
        if slug.is_empty() || slugs.iter().any(|existing| existing == slug) {
            continue;
        }
        slugs.push(slug.to_string());
        if slugs.len() >= limit {
            break;
        }
    }

    slugs
}

fn match_openrouter_rankings_to_model_ids(
    ranked_slugs: &[String],
    catalog: &[OpenRouterModelSummary],
    limit: usize,
) -> Vec<String> {
    let mut by_slug = BTreeMap::new();
    for entry in catalog {
        by_slug.insert(entry.id.to_ascii_lowercase(), entry.id.clone());
        by_slug.insert(entry.canonical_slug.to_ascii_lowercase(), entry.id.clone());
    }

    let mut matched = Vec::new();
    for slug in ranked_slugs {
        let normalized = slug.trim().trim_matches('/').to_ascii_lowercase();
        let Some(model_id) = by_slug.get(&normalized) else {
            continue;
        };
        if matched.iter().any(|existing| existing == model_id) {
            continue;
        }
        matched.push(model_id.clone());
        if matched.len() >= limit {
            break;
        }
    }

    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_openrouter_ranking_slugs_reads_ordered_model_links() {
        let html = r#"
            <html>
              <body>
                <div>ignore me</div>
                <div class="grid grid-cols-12 items-center">
                  <a href="/qwen/qwen3.6-plus-04-02">Qwen 3.6 Plus</a>
                  <a href="/anthropic/claude-4.6-sonnet-20260217">Claude Sonnet 4.6</a>
                  <a href="/anthropic">Anthropic</a>
                  <a href="/openai/gpt-5.4-mini">GPT-5.4 Mini</a>
                </div>
              </body>
            </html>
        "#;

        assert_eq!(
            parse_openrouter_ranking_slugs(html, 3),
            vec![
                "qwen/qwen3.6-plus-04-02".to_string(),
                "anthropic/claude-4.6-sonnet-20260217".to_string(),
                "openai/gpt-5.4-mini".to_string(),
            ]
        );
    }

    #[test]
    fn test_match_openrouter_rankings_to_model_ids_prefers_canonical_slug_matches() {
        let catalog = vec![
            OpenRouterModelSummary {
                id: "qwen/qwen3.6-plus".to_string(),
                canonical_slug: "qwen/qwen3.6-plus-04-02".to_string(),
            },
            OpenRouterModelSummary {
                id: "anthropic/claude-sonnet-4.6".to_string(),
                canonical_slug: "anthropic/claude-4.6-sonnet-20260217".to_string(),
            },
        ];

        assert_eq!(
            match_openrouter_rankings_to_model_ids(
                &[
                    "qwen/qwen3.6-plus-04-02".to_string(),
                    "anthropic/claude-4.6-sonnet-20260217".to_string(),
                ],
                &catalog,
                5,
            ),
            vec![
                "qwen/qwen3.6-plus".to_string(),
                "anthropic/claude-sonnet-4.6".to_string(),
            ]
        );
    }

    #[test]
    fn test_discover_install_models_uses_curated_fallback_when_live_fetch_fails_without_cache() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("openrouter-models.json");

        let discovered =
            discover_install_openrouter_models_with_fetcher(&cache_path, Some("test-key"), |_| {
                Err(anyhow::anyhow!("network down"))
            })
            .unwrap();

        assert_eq!(discovered.source, OpenRouterCatalogSource::CuratedFallback);
        assert_eq!(discovered.models, curated_openrouter_models());
        assert_eq!(discovered.live_error, Some("network down".to_string()));
    }

    #[test]
    fn test_discover_install_models_uses_stale_cache_when_live_fetch_fails() {
        let temp = TempDir::new().unwrap();
        let cache_path = temp.path().join("openrouter-models.json");
        save_cached_openrouter_models(
            &cache_path,
            &[
                "minimax/minimax-m2.7".to_string(),
                "qwen/qwen3.6-plus".to_string(),
            ],
        )
        .unwrap();

        let discovered =
            discover_install_openrouter_models_with_fetcher(&cache_path, Some("test-key"), |_| {
                Err(anyhow::anyhow!("timeout"))
            })
            .unwrap();

        match discovered.source {
            OpenRouterCatalogSource::Cache { age_secs } => assert!(age_secs <= 1),
            other => panic!("expected cached source, got {:?}", other),
        }
        assert_eq!(
            discovered.models,
            vec![
                "minimax/minimax-m2.7".to_string(),
                "qwen/qwen3.6-plus".to_string()
            ]
        );
        assert_eq!(discovered.live_error, Some("timeout".to_string()));
    }
}

use topagent_core::{model::ModelRoute, ProviderKind};

/// Canonical form of an allowed Telegram DM username.
///
/// Telegram usernames are case-insensitive and operators commonly prefix them
/// with `@`. Apply the same normalization at every read and write boundary so
/// the install-time stored form always equals the runtime-compared form.
pub(crate) fn canonicalize_allowed_username(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let stripped = trimmed.trim_start_matches('@').to_lowercase();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectedProvider {
    OpenRouter,
    Opencode,
}

impl SelectedProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Opencode => "Opencode",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "openrouter" | "open router" => Some(Self::OpenRouter),
            "opencode" => Some(Self::Opencode),
            _ => None,
        }
    }

    pub fn to_provider_kind(self) -> ProviderKind {
        match self {
            Self::OpenRouter => ProviderKind::OpenRouter,
            Self::Opencode => ProviderKind::Opencode,
        }
    }

    pub fn from_provider_kind(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenRouter => Self::OpenRouter,
            ProviderKind::Opencode => Self::Opencode,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelResolutionSource {
    CliOverride,
    InteractiveSelection,
    PersistedDefault,
    BuiltInFallback,
}

impl ModelResolutionSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CliOverride => "CLI override",
            Self::InteractiveSelection => "interactive selection",
            Self::PersistedDefault => "persisted default",
            Self::BuiltInFallback => "built-in default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedModel {
    pub provider: ProviderKind,
    pub model_id: String,
    pub source: ModelResolutionSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeModelSelection {
    pub configured_default: ResolvedModel,
    pub effective: ResolvedModel,
}

/// Resolve the effective provider from an explicit config or the current
/// built-in default for uninstalled/local CLI use.
pub(crate) fn provider_or_default(explicit: Option<SelectedProvider>) -> ProviderKind {
    explicit
        .map(|p| p.to_provider_kind())
        .unwrap_or(ProviderKind::OpenRouter)
}

pub(crate) fn resolve_model_choice(
    provider: ProviderKind,
    explicit_model: Option<String>,
    interactive_selection: Option<String>,
    persisted_model: Option<String>,
) -> ResolvedModel {
    use crate::config::defaults::normalize_nonempty_string;

    if let Some(model_id) = normalize_nonempty_string(explicit_model) {
        return ResolvedModel {
            provider,
            model_id,
            source: ModelResolutionSource::CliOverride,
        };
    }

    if let Some(model_id) = normalize_nonempty_string(interactive_selection) {
        return ResolvedModel {
            provider,
            model_id,
            source: ModelResolutionSource::InteractiveSelection,
        };
    }

    if let Some(model_id) = normalize_nonempty_string(persisted_model) {
        return ResolvedModel {
            provider,
            model_id,
            source: ModelResolutionSource::PersistedDefault,
        };
    }

    ResolvedModel {
        provider,
        model_id: provider.default_model_id().to_string(),
        source: ModelResolutionSource::BuiltInFallback,
    }
}

pub(crate) fn resolve_runtime_model_selection(
    provider: ProviderKind,
    explicit_model: Option<String>,
    persisted_model: Option<String>,
) -> RuntimeModelSelection {
    let configured_default = resolve_model_choice(provider, None, None, persisted_model.clone());
    let effective = resolve_model_choice(provider, explicit_model, None, persisted_model);
    RuntimeModelSelection {
        configured_default,
        effective,
    }
}

pub(crate) fn build_route_from_resolved(model: &ResolvedModel) -> ModelRoute {
    // Provider is explicit in the resolved model — no string-prefix inference.
    ModelRoute::new(model.provider, &model.model_id)
}

pub(crate) fn current_configured_model(
    provider: ProviderKind,
    persisted_model: Option<String>,
) -> ResolvedModel {
    resolve_model_choice(provider, None, None, persisted_model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use topagent_core::model::DEFAULT_OPENROUTER_MODEL_ID;

    #[test]
    fn test_canonicalize_allowed_username_strips_at_lowercases_and_trims() {
        assert_eq!(
            canonicalize_allowed_username("@MyUser"),
            Some("myuser".to_string())
        );
        assert_eq!(
            canonicalize_allowed_username("MyUser"),
            Some("myuser".to_string())
        );
        assert_eq!(
            canonicalize_allowed_username("  @@MIXED  "),
            Some("mixed".to_string())
        );
        assert_eq!(canonicalize_allowed_username("   "), None);
        assert_eq!(canonicalize_allowed_username("@"), None);
        assert_eq!(canonicalize_allowed_username(""), None);
    }

    #[test]
    fn test_canonicalize_allowed_username_is_idempotent() {
        let first = canonicalize_allowed_username("@SomeUser").unwrap();
        let second = canonicalize_allowed_username(&first).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_resolve_model_choice_prefers_explicit_then_selected_then_persisted() {
        let resolved = resolve_model_choice(
            ProviderKind::OpenRouter,
            Some(" explicit/model ".to_string()),
            Some("selected/model".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.model_id, "explicit/model");
        assert_eq!(resolved.provider, ProviderKind::OpenRouter);
        assert_eq!(resolved.source, ModelResolutionSource::CliOverride);

        let resolved = resolve_model_choice(
            ProviderKind::OpenRouter,
            Some("   ".to_string()),
            Some(" selected/model ".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.model_id, "selected/model");
        assert_eq!(resolved.source, ModelResolutionSource::InteractiveSelection);

        let resolved = resolve_model_choice(
            ProviderKind::OpenRouter,
            None,
            None,
            Some(" persisted/model ".to_string()),
        );
        assert_eq!(resolved.model_id, "persisted/model");
        assert_eq!(resolved.source, ModelResolutionSource::PersistedDefault);

        let resolved = resolve_model_choice(ProviderKind::OpenRouter, None, None, None);
        assert_eq!(resolved.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(resolved.source, ModelResolutionSource::BuiltInFallback);
    }

    #[test]
    fn test_resolve_runtime_model_selection_tracks_configured_and_effective_models() {
        let resolved = resolve_runtime_model_selection(
            ProviderKind::OpenRouter,
            Some(" explicit/model ".to_string()),
            Some("persisted/model".to_string()),
        );
        assert_eq!(resolved.configured_default.model_id, "persisted/model");
        assert_eq!(
            resolved.configured_default.source,
            ModelResolutionSource::PersistedDefault
        );
        assert_eq!(resolved.effective.model_id, "explicit/model");
        assert_eq!(
            resolved.effective.source,
            ModelResolutionSource::CliOverride
        );

        let resolved = resolve_runtime_model_selection(ProviderKind::OpenRouter, None, None);
        assert_eq!(
            resolved.configured_default.model_id,
            DEFAULT_OPENROUTER_MODEL_ID
        );
        assert_eq!(resolved.effective.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(
            resolved.configured_default.source,
            ModelResolutionSource::BuiltInFallback
        );
        assert_eq!(
            resolved.effective.source,
            ModelResolutionSource::BuiltInFallback
        );
    }

    #[test]
    fn test_build_route_from_resolved_uses_resolved_model_id() {
        let resolved = ResolvedModel {
            provider: ProviderKind::OpenRouter,
            model_id: "custom/model".to_string(),
            source: ModelResolutionSource::CliOverride,
        };
        let route = build_route_from_resolved(&resolved);
        assert_eq!(route.model_id, "custom/model");
        assert_eq!(route.provider, ProviderKind::OpenRouter);
    }

    #[test]
    fn test_current_configured_model_uses_persisted_then_built_in_fallback() {
        let configured = current_configured_model(
            ProviderKind::OpenRouter,
            Some("persisted/model".to_string()),
        );
        assert_eq!(configured.model_id, "persisted/model");
        assert_eq!(configured.source, ModelResolutionSource::PersistedDefault);

        let fallback = current_configured_model(ProviderKind::OpenRouter, None);
        assert_eq!(fallback.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(fallback.source, ModelResolutionSource::BuiltInFallback);
    }

    #[test]
    fn test_explicit_opencode_provider_routes_qwen_correctly() {
        let selection = resolve_runtime_model_selection(
            ProviderKind::Opencode,
            None,
            Some("qwen/qwen3.6-plus".to_string()),
        );
        assert_eq!(selection.effective.provider, ProviderKind::Opencode);
        assert_eq!(selection.effective.model_id, "qwen/qwen3.6-plus");
        let route = build_route_from_resolved(&selection.effective);
        assert_eq!(route.provider, ProviderKind::Opencode);
        assert_eq!(route.model_id, "qwen/qwen3.6-plus");
    }

    #[test]
    fn test_explicit_openrouter_provider_routes_glm4_correctly() {
        let selection = resolve_runtime_model_selection(
            ProviderKind::OpenRouter,
            None,
            Some("glm-4".to_string()),
        );
        assert_eq!(selection.effective.provider, ProviderKind::OpenRouter);
        let route = build_route_from_resolved(&selection.effective);
        assert_eq!(route.provider, ProviderKind::OpenRouter);
    }

    #[test]
    fn test_empty_persisted_model_falls_back_to_built_in_default() {
        use crate::config::defaults::{TelegramModeDefaults, TOPAGENT_MODEL_KEY};
        use std::collections::HashMap;
        let values = HashMap::from([(TOPAGENT_MODEL_KEY.to_string(), "   ".to_string())]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        assert!(
            defaults.model.is_none(),
            "whitespace-only model should parse as None"
        );
        let selection =
            resolve_runtime_model_selection(ProviderKind::OpenRouter, None, defaults.model);
        assert_eq!(
            selection.configured_default.model_id,
            DEFAULT_OPENROUTER_MODEL_ID
        );
        assert_eq!(selection.effective.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }
}

pub const DEFAULT_OPENROUTER_MODEL_ID: &str = "minimax/minimax-m2.7";
pub const DEFAULT_OPENCODE_MODEL_ID: &str = "glm-5.1";

pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const OPENCODE_BASE_URL: &str = "https://api.opencode.ai/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    OpenRouter,
    Opencode,
}

impl ProviderKind {
    pub fn base_url(&self) -> &'static str {
        match self {
            ProviderKind::OpenRouter => OPENROUTER_BASE_URL,
            ProviderKind::Opencode => OPENCODE_BASE_URL,
        }
    }

    pub fn default_model_id(&self) -> &'static str {
        match self {
            ProviderKind::OpenRouter => DEFAULT_OPENROUTER_MODEL_ID,
            ProviderKind::Opencode => DEFAULT_OPENCODE_MODEL_ID,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ProviderKind::OpenRouter => "OpenRouter",
            ProviderKind::Opencode => "Opencode",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub provider: ProviderKind,
    pub model_id: String,
}

impl ModelRoute {
    pub fn new(provider: ProviderKind, model_id: impl Into<String>) -> Self {
        Self {
            provider,
            model_id: model_id.into(),
        }
    }

    pub fn openrouter(model_id: impl Into<String>) -> Self {
        Self::new(ProviderKind::OpenRouter, model_id)
    }

    pub fn opencode(model_id: impl Into<String>) -> Self {
        Self::new(ProviderKind::Opencode, model_id)
    }
}

impl Default for ModelRoute {
    fn default() -> Self {
        Self::openrouter(DEFAULT_OPENROUTER_MODEL_ID)
    }
}

impl ModelRoute {
    pub fn with_override(provider: ProviderKind, model_override: Option<&str>) -> Self {
        let model_id = model_override.unwrap_or(provider.default_model_id());
        Self::new(provider, model_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_route_is_openrouter() {
        let route = ModelRoute::default();
        assert_eq!(route.provider, ProviderKind::OpenRouter);
        assert_eq!(route.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }

    #[test]
    fn test_model_route_equality() {
        let route1 = ModelRoute::openrouter("model-1");
        let route2 = ModelRoute::openrouter("model-1");
        let route3 = ModelRoute::openrouter("model-2");
        assert_eq!(route1, route2);
        assert_ne!(route1, route3);
    }

    #[test]
    fn test_opencode_route() {
        let route = ModelRoute::opencode("glm-5.1");
        assert_eq!(route.provider, ProviderKind::Opencode);
        assert_eq!(route.model_id, "glm-5.1");
    }

    #[test]
    fn test_provider_kind_base_url() {
        assert_eq!(ProviderKind::OpenRouter.base_url(), OPENROUTER_BASE_URL);
        assert_eq!(ProviderKind::Opencode.base_url(), OPENCODE_BASE_URL);
    }

    #[test]
    fn test_with_override_uses_default_model() {
        let route = ModelRoute::with_override(ProviderKind::OpenRouter, None);
        assert_eq!(route.model_id, DEFAULT_OPENROUTER_MODEL_ID);
        assert_eq!(route.provider, ProviderKind::OpenRouter);
    }

    #[test]
    fn test_with_override_uses_custom_model() {
        let route = ModelRoute::with_override(ProviderKind::OpenRouter, Some("custom/model"));
        assert_eq!(route.model_id, "custom/model");
    }

    #[test]
    fn test_with_override_opencode_default() {
        let route = ModelRoute::with_override(ProviderKind::Opencode, None);
        assert_eq!(route.model_id, DEFAULT_OPENCODE_MODEL_ID);
        assert_eq!(route.provider, ProviderKind::Opencode);
    }

    #[test]
    fn test_provider_kind_display() {
        assert_eq!(format!("{}", ProviderKind::OpenRouter), "OpenRouter");
        assert_eq!(format!("{}", ProviderKind::Opencode), "Opencode");
    }

    #[test]
    fn test_dual_provider_seam_uses_distinct_base_urls() {
        let openrouter_url = ProviderKind::OpenRouter.base_url();
        let opencode_url = ProviderKind::Opencode.base_url();
        assert_ne!(
            openrouter_url, opencode_url,
            "providers must not share a base URL"
        );
        assert!(
            openrouter_url.starts_with("https://"),
            "OpenRouter base URL must be HTTPS"
        );
        assert!(
            opencode_url.starts_with("https://"),
            "Opencode base URL must be HTTPS"
        );
    }

    #[test]
    fn test_opencode_route_uses_opencode_provider_kind() {
        let route = ModelRoute::opencode("glm-5.1");
        assert_eq!(route.provider, ProviderKind::Opencode);
        assert_eq!(route.model_id, "glm-5.1");
        assert_eq!(route.provider.base_url(), OPENCODE_BASE_URL);
    }

    #[test]
    fn test_model_route_preserves_explicit_model_id_across_providers() {
        let or_route = ModelRoute::openrouter("anthropic/claude-sonnet-4.6");
        let oc_route = ModelRoute::opencode("qwen/qwen3.6-plus");
        assert_eq!(or_route.model_id, "anthropic/claude-sonnet-4.6");
        assert_eq!(oc_route.model_id, "qwen/qwen3.6-plus");
        assert_eq!(or_route.provider, ProviderKind::OpenRouter);
        assert_eq!(oc_route.provider, ProviderKind::Opencode);
    }

    #[test]
    fn test_with_override_switches_provider_kind() {
        let route =
            ModelRoute::with_override(ProviderKind::Opencode, Some("custom-opencode-model"));
        assert_eq!(route.provider, ProviderKind::Opencode);
        assert_eq!(route.model_id, "custom-opencode-model");
    }
}

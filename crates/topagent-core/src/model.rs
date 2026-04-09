pub const DEFAULT_OPENROUTER_MODEL_ID: &str = "minimax/minimax-m2.7";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub model_id: String,
}

impl ModelRoute {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
        }
    }
}

impl Default for ModelRoute {
    fn default() -> Self {
        Self::openrouter(DEFAULT_OPENROUTER_MODEL_ID)
    }
}

impl ModelRoute {
    pub fn openrouter(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
        }
    }
}

impl ModelRoute {
    /// Build a route with an optional model override, falling back to the default model.
    pub fn with_override(model_override: Option<&str>) -> Self {
        Self::openrouter(model_override.unwrap_or(DEFAULT_OPENROUTER_MODEL_ID))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_route() {
        let route = ModelRoute::default();
        assert_eq!(route.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }

    #[test]
    fn test_model_route_equality() {
        let route1 = ModelRoute::new("model-1");
        let route2 = ModelRoute::new("model-1");
        let route3 = ModelRoute::new("model-2");
        assert_eq!(route1, route2);
        assert_ne!(route1, route3);
    }

    #[test]
    fn test_with_override_uses_default_model() {
        let route = ModelRoute::with_override(None);
        assert_eq!(route.model_id, DEFAULT_OPENROUTER_MODEL_ID);
    }

    #[test]
    fn test_with_override_uses_custom_model() {
        let route = ModelRoute::with_override(Some("custom/model"));
        assert_eq!(route.model_id, "custom/model");
    }
}

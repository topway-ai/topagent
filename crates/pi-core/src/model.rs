#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub provider_id: ProviderId,
    pub model_id: String,
}

impl ModelRoute {
    pub fn new(provider_id: ProviderId, model_id: impl Into<String>) -> Self {
        Self {
            provider_id,
            model_id: model_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderId {
    OpenRouter,
}

impl ProviderId {
    pub fn as_str(&self) -> &str {
        match self {
            ProviderId::OpenRouter => "openrouter",
        }
    }
}

impl Default for ModelRoute {
    fn default() -> Self {
        Self::openrouter("minimax/minimax-m2.7")
    }
}

impl ModelRoute {
    pub fn openrouter(model_id: impl Into<String>) -> Self {
        Self {
            provider_id: ProviderId::OpenRouter,
            model_id: model_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_route() {
        let route = ModelRoute::default();
        assert_eq!(route.provider_id, ProviderId::OpenRouter);
        assert_eq!(route.model_id, "minimax/minimax-m2.7");
    }

    #[test]
    fn test_provider_id_as_str() {
        assert_eq!(ProviderId::OpenRouter.as_str(), "openrouter");
    }

    #[test]
    fn test_model_route_equality() {
        let route1 = ModelRoute::new(ProviderId::OpenRouter, "model-1");
        let route2 = ModelRoute::new(ProviderId::OpenRouter, "model-1");
        let route3 = ModelRoute::new(ProviderId::OpenRouter, "model-2");
        assert_eq!(route1, route2);
        assert_ne!(route1, route3);
    }
}

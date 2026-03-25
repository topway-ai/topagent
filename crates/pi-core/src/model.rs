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

    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "openrouter" => Ok(ProviderId::OpenRouter),
            _ => Err(format!("unknown provider '{}'", s)),
        }
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCategory {
    Default,
    Summarization,
    EditMutation,
    Review,
}

impl TaskCategory {
    pub fn all() -> &'static [Self] {
        &[
            Self::Default,
            Self::Summarization,
            Self::EditMutation,
            Self::Review,
        ]
    }
}

pub struct RoutingPolicy;

impl RoutingPolicy {
    pub fn select_route(category: TaskCategory, model_override: Option<&str>) -> ModelRoute {
        let model = model_override.unwrap_or("minimax/minimax-m2.7");
        match category {
            TaskCategory::Default => ModelRoute::openrouter(model),
            TaskCategory::Summarization => ModelRoute::openrouter(model),
            TaskCategory::EditMutation => ModelRoute::openrouter(model),
            TaskCategory::Review => ModelRoute::openrouter(model),
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

    #[test]
    fn test_provider_id_parse_openrouter() {
        let result = ProviderId::parse("openrouter");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ProviderId::OpenRouter);
    }

    #[test]
    fn test_provider_id_parse_case_insensitive() {
        assert!(ProviderId::parse("OpenRouter").is_ok());
        assert!(ProviderId::parse("OPENROUTER").is_ok());
    }

    #[test]
    fn test_provider_id_parse_unknown() {
        let result = ProviderId::parse("unknown");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown provider"));
    }

    #[test]
    fn test_provider_id_display() {
        assert_eq!(format!("{}", ProviderId::OpenRouter), "openrouter");
    }

    #[test]
    fn test_routing_policy_default_route() {
        let route = RoutingPolicy::select_route(TaskCategory::Default, None);
        assert_eq!(route.provider_id, ProviderId::OpenRouter);
        assert_eq!(route.model_id, "minimax/minimax-m2.7");
    }

    #[test]
    fn test_routing_policy_with_model_override() {
        let route = RoutingPolicy::select_route(TaskCategory::Default, Some("custom/model"));
        assert_eq!(route.model_id, "custom/model");
    }

    #[test]
    fn test_routing_policy_all_categories_return_valid_route() {
        for &category in TaskCategory::all() {
            let route = RoutingPolicy::select_route(category, None);
            assert_eq!(route.provider_id, ProviderId::OpenRouter);
        }
    }

    #[test]
    fn test_task_category_all_has_four_values() {
        assert_eq!(TaskCategory::all().len(), 4);
    }
}

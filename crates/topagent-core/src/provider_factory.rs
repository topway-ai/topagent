use crate::model::{ModelRoute, ProviderId};
use crate::openrouter::OpenRouterProvider;
use crate::{Provider, Result, ToolSpec};

pub fn create_provider(
    route: &ModelRoute,
    api_key: &str,
    tools: Vec<ToolSpec>,
    timeout_secs: u64,
) -> Result<Box<dyn Provider>> {
    let _ = &route.model_id; // model is determined by route at call time
    match route.provider_id {
        ProviderId::OpenRouter => Ok(Box::new(OpenRouterProvider::with_tools_and_timeout(
            api_key,
            tools,
            timeout_secs,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelRoute;

    #[test]
    fn test_create_openrouter_provider() {
        let route = ModelRoute::default();
        let provider = create_provider(&route, "test-key", vec![], 120);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_provider_with_custom_model() {
        let route = ModelRoute::new(ProviderId::OpenRouter, "anthropic/claude-3");
        let provider = create_provider(&route, "test-key", vec![], 120);
        assert!(provider.is_ok());
    }
}

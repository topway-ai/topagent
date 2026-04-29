use crate::capability::{AccessMode, CapabilityKind, CapabilityRequest, RiskLevel};
use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

const MAX_QUERY_CHARS: usize = 240;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchArgs {
    pub query: String,
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Clone)]
pub struct WebSearchTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".to_string(),
            description: "search the web for bounded low-trust text results; not implemented in this build, so no network request is made".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "search query"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "maximum result count requested by the model; bounded by the implementation"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: WebSearchArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(Error::InvalidInput(
                "web_search query cannot be empty".to_string(),
            ));
        }

        ctx.authorize_capability(CapabilityRequest::new(
            CapabilityKind::WebSearch,
            "web_search",
            AccessMode::Read,
            RiskLevel::Safe,
            format!(
                "web_search query `{}`; remote content is low trust and must not be executed",
                compact(query, 120)
            ),
        ))?;
        ctx.authorize_capability(CapabilityRequest::new(
            CapabilityKind::Network,
            "web_search",
            AccessMode::Read,
            RiskLevel::Safe,
            "web_search requires network access",
        ))?;

        Ok(format!(
            "web_search is not implemented in this build. No network request was made, no remote content was executed, and no durable memory was written.\nquery: {}\nmax_results_requested: {}",
            compact(query, MAX_QUERY_CHARS),
            args.max_results.unwrap_or(5).min(5)
        ))
    }
}

fn compact(value: &str, max_len: usize) -> String {
    let compacted = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.len() <= max_len {
        compacted
    } else {
        format!("{}...", &compacted[..max_len.saturating_sub(3)])
    }
}

use crate::capability::{assess_computer_action, AccessMode, CapabilityKind, CapabilityRequest};
use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseAction {
    Observe,
    Navigate,
    Click,
    Type,
    Scroll,
}

impl ComputerUseAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Navigate => "navigate",
            Self::Click => "click",
            Self::Type => "type",
            Self::Scroll => "scroll",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerUseArgs {
    pub action: ComputerUseAction,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub x: Option<i64>,
    #[serde(default)]
    pub y: Option<i64>,
}

#[derive(Clone)]
pub struct ComputerUseTool;

impl ComputerUseTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ComputerUseTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for ComputerUseTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "computer_use".to_string(),
            description: "controlled computer-use scaffold for observe, navigate, click, type, and scroll actions; disabled unless the access profile or an explicit grant allows computer_use".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["observe", "navigate", "click", "type", "scroll"]
                    },
                    "target": {
                        "type": "string",
                        "description": "URL, UI target, or page description for the action"
                    },
                    "text": {
                        "type": "string",
                        "description": "text to type when action=type"
                    },
                    "x": {"type": "integer", "description": "x coordinate for click"},
                    "y": {"type": "integer", "description": "y coordinate for click"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: ComputerUseArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;
        validate_args(&args)?;
        let target = args
            .target
            .as_deref()
            .or(args.text.as_deref())
            .unwrap_or(args.action.as_str());
        let (risk, reason) = assess_computer_action(args.action.as_str(), target);
        ctx.authorize_capability(CapabilityRequest::new(
            CapabilityKind::ComputerUse,
            target.to_string(),
            AccessMode::Execute,
            risk,
            reason,
        ))?;

        let session_dir = ctx
            .workspace_root()
            .join(".topagent")
            .join("computer-use-session");
        std::fs::create_dir_all(&session_dir)?;

        Ok(format!(
            "computer_use scaffold accepted action `{}` for `{}` using isolated session directory {}. No desktop provider is wired in this build, so no UI action was performed.",
            args.action.as_str(),
            target,
            session_dir.display()
        ))
    }
}

fn validate_args(args: &ComputerUseArgs) -> Result<()> {
    match args.action {
        ComputerUseAction::Click => {
            if args.x.is_none() || args.y.is_none() {
                return Err(Error::InvalidInput(
                    "computer_use click requires x and y coordinates".to_string(),
                ));
            }
        }
        ComputerUseAction::Type => {
            if args.text.as_deref().unwrap_or("").is_empty() {
                return Err(Error::InvalidInput(
                    "computer_use type requires non-empty text".to_string(),
                ));
            }
        }
        ComputerUseAction::Navigate => {
            if args.target.as_deref().unwrap_or("").is_empty() {
                return Err(Error::InvalidInput(
                    "computer_use navigate requires a target URL".to_string(),
                ));
            }
        }
        ComputerUseAction::Observe | ComputerUseAction::Scroll => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{AccessConfig, CapabilityManager, CapabilityProfile};
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use tempfile::TempDir;

    #[test]
    fn test_computer_use_denied_under_developer_profile() {
        let temp = TempDir::new().unwrap();
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let exec =
            ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = ComputerUseTool::new().execute(serde_json::json!({"action": "observe"}), &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("computer_use"));
    }
}

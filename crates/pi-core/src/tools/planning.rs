use crate::context::ToolContext;
use crate::plan::{Plan, TodoItem, TodoStatus};
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlanArgs {
    pub items: Vec<PlanItem>,
}

pub struct UpdatePlanTool {
    agent_plan: Option<std::sync::Arc<std::sync::Mutex<Plan>>>,
}

impl UpdatePlanTool {
    pub fn new() -> Self {
        Self { agent_plan: None }
    }

    pub fn with_plan(plan: std::sync::Arc<std::sync::Mutex<Plan>>) -> Self {
        Self {
            agent_plan: Some(plan),
        }
    }

    pub fn bind_plan(&mut self, plan: std::sync::Arc<std::sync::Mutex<Plan>>) {
        self.agent_plan = Some(plan);
    }
}

impl Default for UpdatePlanTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for UpdatePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_plan".to_string(),
            description: "Create or replace the current plan with a list of items. Each item has content and status (pending/in_progress/done). Use this for multi-step tasks to track progress.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "inprogress", "done"]}
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: UpdatePlanArgs =
            serde_json::from_value(args).map_err(|e| Error::InvalidInput(e.to_string()))?;

        let plan = self
            .agent_plan
            .as_ref()
            .ok_or_else(|| Error::ToolFailed("update_plan: no plan bound".to_string()))?;

        let mut plan_guard = plan
            .lock()
            .map_err(|e| Error::ToolFailed(format!("update_plan: lock failed: {}", e)))?;

        let items: Vec<TodoItem> = args
            .items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| TodoItem {
                id: idx,
                description: item.content,
                status: item.status,
            })
            .collect();

        plan_guard.clear();
        plan_guard.set_items(items);

        Ok(plan_guard.format_for_display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use std::sync::Arc;

    #[test]
    fn test_update_plan_creates_plan() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "First task", "status": "pending"},
                {"content": "Second task", "status": "pending"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("First task"));
        assert!(output.contains("Second task"));
    }

    #[test]
    fn test_update_plan_updates_existing() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        plan.lock().unwrap().add_item("Old task".to_string());

        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "New task", "status": "inprogress"}
            ]
        });

        let result = tool.execute(args, &ctx);
        if result.is_err() {
            eprintln!("Error: {:?}", result);
        }
        assert!(result.is_ok());

        let plan_guard = plan.lock().unwrap();
        assert_eq!(plan_guard.items().len(), 1);
        assert_eq!(plan_guard.items()[0].description, "New task");
    }
}

use crate::context::ToolContext;
use crate::plan::{CodingWorkflowKind, Plan, TodoItem, TodoStatus};
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub kind: Option<CodingWorkflowKind>,
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
                                "status": {"type": "string", "enum": ["pending", "in_progress", "blocked", "done"]},
                                "kind": {
                                    "type": "string",
                                    "enum": ["audit", "patch", "test", "commit_review", "release_gate"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<String> {
        let args: UpdatePlanArgs = serde_json::from_value(args)
            .map_err(|e| Error::InvalidInput(format!("update_plan: invalid input: {}", e)))?;

        let plan = self
            .agent_plan
            .as_ref()
            .ok_or_else(|| Error::ToolFailed("update_plan: plan not initialized".to_string()))?;

        let mut plan_guard = plan.lock().map_err(|e| {
            Error::ToolFailed(format!("update_plan: cannot acquire plan lock: {}", e))
        })?;

        let items: Vec<TodoItem> = args
            .items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| TodoItem {
                id: idx,
                description: item.content,
                status: item.status,
                workflow_kind: item.kind,
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
                {"content": "New task", "status": "in_progress"}
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

    #[test]
    fn test_update_plan_accepts_coding_workflow_kind() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "Run release gate", "status": "pending", "kind": "release_gate"}
            ]
        });

        let output = tool.execute(args, &ctx).unwrap();
        let plan_guard = plan.lock().unwrap();

        assert_eq!(
            plan_guard.items()[0].workflow_kind,
            Some(CodingWorkflowKind::ReleaseGate)
        );
        assert!(output.contains("release_gate: Run release gate"));
    }

    #[test]
    fn test_update_plan_canonical_in_progress_used_in_output() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "Canonical task", "status": "in_progress"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            !output.contains("in_progress"),
            "output should use symbols, not string"
        );
        assert!(
            output.contains("[>]"),
            "canonical in_progress should render as symbol"
        );
    }

    #[test]
    fn test_update_plan_clears_with_empty_items() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        plan.lock().unwrap().add_item("Old task".to_string());
        assert!(!plan.lock().unwrap().is_empty());

        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": []
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        assert!(plan.lock().unwrap().is_empty());
        let output = result.unwrap();
        assert!(output.contains("no plan"));
    }

    #[test]
    fn test_update_plan_rejects_invalid_status() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "Bad status task", "status": "invalid_status"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid") || err.to_string().contains("InvalidInput"));
    }

    #[test]
    fn test_update_plan_replace_removes_stale_items() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        plan.lock().unwrap().add_item("Old task 1".to_string());
        plan.lock().unwrap().add_item("Old task 2".to_string());
        assert_eq!(plan.lock().unwrap().items().len(), 2);

        let tool = UpdatePlanTool::with_plan(plan.clone());
        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "New task", "status": "pending"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());

        let plan_guard = plan.lock().unwrap();
        assert_eq!(plan_guard.items().len(), 1);
        assert_eq!(plan_guard.items()[0].description, "New task");
    }

    #[test]
    fn test_update_plan_empty_items_clears_plan() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        plan.lock().unwrap().add_item("Task to clear".to_string());
        assert!(!plan.lock().unwrap().is_empty());

        let tool = UpdatePlanTool::with_plan(plan.clone());
        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({ "items": [] });
        let result = tool.execute(args, &ctx);

        assert!(result.is_ok());
        assert!(plan.lock().unwrap().is_empty());
    }

    #[test]
    fn test_update_plan_rejects_missing_content_field() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"status": "pending"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("update_plan"),
            "error should mention tool name"
        );
    }

    #[test]
    fn test_update_plan_rejects_missing_status_field() {
        let plan = Arc::new(std::sync::Mutex::new(Plan::new()));
        let tool = UpdatePlanTool::with_plan(plan.clone());

        let exec = crate::context::ExecutionContext::new(std::path::PathBuf::from("/tmp"));
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let args = serde_json::json!({
            "items": [
                {"content": "task without status"}
            ]
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("update_plan"),
            "error should mention tool name"
        );
    }
}

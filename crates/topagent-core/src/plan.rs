use crate::behavior::BehaviorContract;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TodoStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "done")]
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub description: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    items: Vec<TodoItem>,
    next_id: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMode {
    PlanAndExecute,
    InspectOnly,
    VerifyOnly,
}

impl Plan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_item(&mut self, description: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(TodoItem {
            id,
            description,
            status: TodoStatus::Pending,
        });
        id
    }

    pub fn update_status(&mut self, id: usize, status: TodoStatus) -> bool {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.status = status;
            true
        } else {
            false
        }
    }

    pub fn mark_in_progress(&mut self, id: usize) -> bool {
        self.update_status(id, TodoStatus::InProgress)
    }

    pub fn mark_done(&mut self, id: usize) -> bool {
        self.update_status(id, TodoStatus::Done)
    }

    pub fn remove_item(&mut self, id: usize) -> bool {
        let len_before = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() < len_before
    }

    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn set_items(&mut self, items: Vec<TodoItem>) {
        self.items = items;
        self.next_id = self.items.len();
    }

    pub fn format_for_display(&self) -> String {
        if self.items.is_empty() {
            return String::from("(no plan)");
        }
        let mut result = String::from("Current plan:\n");
        for item in &self.items {
            let status_symbol = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[>]",
                TodoStatus::Done => "[x]",
            };
            result.push_str(&format!(
                "  {} {} - {}\n",
                status_symbol, item.id, item.description
            ));
        }
        result
    }
}

/// Heuristic fast path for task classification.
///
/// Returns `Some(true)` if the task definitely requires planning,
/// `Some(false)` if it definitely does not, or `None` if the answer is
/// ambiguous and an LLM classification call should be used.
pub fn heuristic_fast_path(instruction: &str) -> Option<bool> {
    BehaviorContract::default().classify_task_fast_path(instruction)
}

pub fn task_mode_fast_path(instruction: &str) -> Option<TaskMode> {
    BehaviorContract::default().task_mode_fast_path(instruction)
}

pub fn build_task_mode_messages(instruction: &str) -> (String, String) {
    BehaviorContract::default().build_task_mode_messages(instruction)
}

pub fn parse_task_mode_response(response: &str) -> Option<TaskMode> {
    let trimmed = response.trim().to_lowercase();
    match trimmed.as_str() {
        "execute" => Some(TaskMode::PlanAndExecute),
        "inspect" => Some(TaskMode::InspectOnly),
        "verify" => Some(TaskMode::VerifyOnly),
        _ => None,
    }
}

/// Build the messages for an LLM classification call.
pub fn build_classification_messages(instruction: &str) -> (String, String) {
    BehaviorContract::default().build_task_classification_messages(instruction)
}

/// Parse the LLM classification response. Returns `true` if planning
/// is required. Defaults to `false` (direct execution) for ambiguous
/// or unparseable responses — it is safer to attempt direct execution
/// and let the model plan voluntarily than to block on a gate.
pub fn parse_classification_response(response: &str) -> bool {
    let trimmed = response.trim().to_lowercase();
    // Accept "plan" anywhere in short responses to handle minor formatting.
    trimmed == "plan" || (trimmed.len() < 20 && trimmed.contains("plan") && !trimmed.contains("no"))
}

/// Build messages for a dedicated plan-generation LLM call.
pub fn build_plan_generation_prompt(instruction: &str) -> (String, String) {
    BehaviorContract::default().build_plan_generation_prompt(instruction)
}

/// Parse the LLM plan-generation response into a list of step descriptions.
/// Extracts lines that look like numbered steps (e.g., "1. Do something").
/// Returns an empty vec if no steps were found.
pub fn parse_plan_generation_response(response: &str) -> Vec<String> {
    response
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            // Match "1. ...", "2) ...", "- ..." style bullets
            let content = if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                // "1. text" or "1) text"
                rest.trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches(['.', ')', ':'])
                    .trim()
            } else if let Some(rest) = trimmed.strip_prefix('-') {
                rest.trim()
            } else {
                return None;
            };
            if content.is_empty() {
                return None;
            }
            Some(content.to_string())
        })
        .take(7) // Cap at 7 steps
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_add_item() {
        let mut plan = Plan::new();
        let id1 = plan.add_item("First task".to_string());
        let id2 = plan.add_item("Second task".to_string());

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(plan.items().len(), 2);
    }

    #[test]
    fn test_plan_update_status() {
        let mut plan = Plan::new();
        let id = plan.add_item("Test task".to_string());

        assert_eq!(plan.items()[0].status, TodoStatus::Pending);

        assert!(plan.mark_in_progress(id));
        assert_eq!(plan.items()[0].status, TodoStatus::InProgress);

        assert!(plan.mark_done(id));
        assert_eq!(plan.items()[0].status, TodoStatus::Done);
    }

    #[test]
    fn test_plan_update_nonexistent_id() {
        let mut plan = Plan::new();
        plan.add_item("Task".to_string());

        assert!(!plan.update_status(999, TodoStatus::Done));
    }

    #[test]
    fn test_plan_remove_item() {
        let mut plan = Plan::new();
        let id = plan.add_item("Task to remove".to_string());
        assert_eq!(plan.items().len(), 1);

        assert!(plan.remove_item(id));
        assert_eq!(plan.items().len(), 0);
    }

    #[test]
    fn test_plan_clear() {
        let mut plan = Plan::new();
        plan.add_item("Task 1".to_string());
        plan.add_item("Task 2".to_string());
        assert_eq!(plan.items().len(), 2);

        plan.clear();
        assert!(plan.is_empty());
    }

    #[test]
    fn test_plan_format_for_display() {
        let mut plan = Plan::new();
        plan.add_item("Task 1".to_string());
        let id2 = plan.add_item("Task 2".to_string());
        plan.mark_in_progress(id2);

        let display = plan.format_for_display();
        assert!(display.contains("[ ]"));
        assert!(display.contains("[>]"));
        assert!(display.contains("Task 1"));
        assert!(display.contains("Task 2"));
    }

    #[test]
    fn test_plan_format_empty() {
        let plan = Plan::new();
        assert_eq!(plan.format_for_display(), "(no plan)");
    }

    #[test]
    fn test_todo_status_deserializes_canonical_in_progress() {
        let json = r#""in_progress""#;
        let status: TodoStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status, TodoStatus::InProgress);
    }

    #[test]
    fn test_todo_status_always_serializes_to_canonical() {
        let status = TodoStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"in_progress\"");
    }

    #[test]
    fn test_format_for_display_uses_symbols_not_strings() {
        let mut plan = Plan::new();
        plan.add_item("Task 1".to_string());
        let id2 = plan.add_item("Task 2".to_string());
        plan.mark_in_progress(id2);
        let id3 = plan.add_item("Task 3".to_string());
        plan.mark_done(id3);

        let display = plan.format_for_display();
        assert!(!display.contains("in_progress"));
        assert!(display.contains("[>]"));
        assert!(display.contains("[x]"));
        assert!(display.contains("[ ]"));
    }

    #[test]
    fn test_plan_replace_removes_old_items() {
        let mut plan = Plan::new();
        plan.add_item("Old task 1".to_string());
        plan.add_item("Old task 2".to_string());

        let new_items = vec![TodoItem {
            id: 0,
            description: "New task 1".to_string(),
            status: TodoStatus::Pending,
        }];
        plan.clear();
        plan.set_items(new_items);

        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.items()[0].description, "New task 1");
    }

    #[test]
    fn test_plan_clear_resets_next_id() {
        let mut plan = Plan::new();
        plan.add_item("Task 1".to_string());
        plan.add_item("Task 2".to_string());
        plan.add_item("Task 3".to_string());
        assert_eq!(plan.next_id, 3);

        plan.clear();
        assert_eq!(plan.next_id, 0);

        let id = plan.add_item("New task".to_string());
        assert_eq!(id, 0, "new items should start from id 0 after clear");
    }

    #[test]
    fn test_empty_plan_format_returns_no_plan_indicator() {
        let plan = Plan::new();
        let display = plan.format_for_display();
        assert_eq!(display, "(no plan)");
    }

    #[test]
    fn test_plan_after_clear_is_empty() {
        let mut plan = Plan::new();
        plan.add_item("Task".to_string());
        assert!(!plan.is_empty());

        plan.clear();
        assert!(plan.is_empty());
    }

    // ── Heuristic fast-path tests ──

    #[test]
    fn test_fast_path_explicit_plan_request() {
        assert_eq!(
            heuristic_fast_path("make a plan for the refactor"),
            Some(true)
        );
        assert_eq!(
            heuristic_fast_path("give me steps to implement this"),
            Some(true)
        );
        assert_eq!(
            heuristic_fast_path("break down this task into steps"),
            Some(true)
        );
    }

    #[test]
    fn test_fast_path_broad_scope() {
        assert_eq!(heuristic_fast_path("refactor the entire repo"), Some(true));
        assert_eq!(
            heuristic_fast_path("fix bugs across the project"),
            Some(true)
        );
        assert_eq!(
            heuristic_fast_path("fix bugs across the codebase"),
            Some(true)
        );
        assert_eq!(
            heuristic_fast_path("refactor the whole project"),
            Some(true)
        );
    }

    #[test]
    fn test_fast_path_trivial_queries() {
        assert_eq!(
            heuristic_fast_path("what is the meaning of life"),
            Some(false)
        );
        assert_eq!(heuristic_fast_path("show me the files"), Some(false));
        assert_eq!(heuristic_fast_path("list all functions"), Some(false));
        assert_eq!(heuristic_fast_path("read this file"), Some(false));
    }

    #[test]
    fn test_fast_path_short_instructions_are_direct() {
        // Short instructions (≤ 120 chars) without broad scope → direct
        assert_eq!(heuristic_fast_path("fix the bug in main.rs"), Some(false));
        assert_eq!(heuristic_fast_path("add a new function"), Some(false));
        assert_eq!(
            heuristic_fast_path("add a comment to this file"),
            Some(false)
        );
        assert_eq!(
            heuristic_fast_path("Fix the typo in main.rs, then show me the diff"),
            Some(false)
        );
    }

    #[test]
    fn test_fast_path_ambiguous_returns_none() {
        // Longer instructions (> 120 chars) without broad scope keywords → ambiguous
        assert_eq!(
            heuristic_fast_path(
                "Add exactly one short single-line comment to the main CLI entry file. Do not rewrite existing comments, do not convert comments to rustdoc, and do not change anything else."
            ),
            None
        );
    }

    // ── LLM classification response parsing ──

    #[test]
    fn test_parse_classification_direct() {
        assert!(!parse_classification_response("direct"));
        assert!(!parse_classification_response("  direct  "));
        assert!(!parse_classification_response("Direct"));
        assert!(!parse_classification_response("DIRECT"));
    }

    #[test]
    fn test_parse_classification_plan() {
        assert!(parse_classification_response("plan"));
        assert!(parse_classification_response("  plan  "));
        assert!(parse_classification_response("Plan"));
        assert!(parse_classification_response("PLAN"));
    }

    #[test]
    fn test_parse_classification_ambiguous_defaults_to_direct() {
        // Unparseable or unexpected responses default to direct execution.
        assert!(!parse_classification_response("I think this needs a plan"));
        assert!(!parse_classification_response("maybe"));
        assert!(!parse_classification_response(""));
        assert!(!parse_classification_response("no plan needed"));
    }

    #[test]
    fn test_classification_prompt_is_well_formed() {
        let (system, user) = build_classification_messages("fix the typo");
        assert!(system.contains("direct"));
        assert!(system.contains("plan"));
        assert_eq!(user, "fix the typo");
    }

    #[test]
    fn test_task_mode_fast_path_detects_mutation() {
        assert_eq!(
            task_mode_fast_path("Make a plan and implement the feature"),
            Some(TaskMode::PlanAndExecute)
        );
    }

    #[test]
    fn test_task_mode_fast_path_defers_non_mutation_tasks() {
        assert_eq!(
            task_mode_fast_path("Make a plan to assess the repository and return findings only"),
            None
        );
    }

    #[test]
    fn test_task_mode_parse_response() {
        assert_eq!(
            parse_task_mode_response("execute"),
            Some(TaskMode::PlanAndExecute)
        );
        assert_eq!(
            parse_task_mode_response("inspect"),
            Some(TaskMode::InspectOnly)
        );
        assert_eq!(
            parse_task_mode_response("verify"),
            Some(TaskMode::VerifyOnly)
        );
        assert_eq!(parse_task_mode_response("unknown"), None);
    }

    #[test]
    fn test_parse_plan_generation_numbered_list() {
        let response = "1. Read src/main.rs\n2. Add the feature\n3. Run tests";
        let items = parse_plan_generation_response(response);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "Read src/main.rs");
        assert_eq!(items[1], "Add the feature");
        assert_eq!(items[2], "Run tests");
    }

    #[test]
    fn test_parse_plan_generation_dash_bullets() {
        let response = "- Investigate the code\n- Make changes\n- Verify";
        let items = parse_plan_generation_response(response);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "Investigate the code");
    }

    #[test]
    fn test_parse_plan_generation_with_preamble() {
        let response = "Here is the plan:\n1. First step\n2. Second step\nGood luck!";
        let items = parse_plan_generation_response(response);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], "First step");
        assert_eq!(items[1], "Second step");
    }

    #[test]
    fn test_parse_plan_generation_empty_response() {
        let items = parse_plan_generation_response("I don't know what to do");
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_plan_generation_capped_at_seven() {
        let response = (1..=10)
            .map(|i| format!("{}. Step {}", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        let items = parse_plan_generation_response(&response);
        assert_eq!(items.len(), 7);
    }

    #[test]
    fn test_parse_plan_generation_parenthesis_format() {
        let response = "1) Read files\n2) Edit code\n3) Test";
        let items = parse_plan_generation_response(response);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "Read files");
    }

    #[test]
    fn test_build_plan_generation_prompt_includes_instruction() {
        let (sys, user) = build_plan_generation_prompt("fix the bug");
        assert!(sys.contains("planning assistant"));
        assert!(user.contains("fix the bug"));
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TodoStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(alias = "inprogress")]
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

fn has_explicit_sequence(lower: &str) -> bool {
    lower.contains(" and then ")
        || lower.contains(" then ")
        || lower.contains(" followed by ")
        || lower.contains(" after that")
        || lower.contains(" next,")
        || lower.contains(" first,")
        || lower.contains(" finally,")
}

fn has_explicit_plan_request(lower: &str) -> bool {
    lower.contains("make a plan")
        || lower.contains("create a plan")
        || lower.contains("give me steps")
        || lower.contains("give me a checklist")
        || lower.contains("break down")
        || lower.contains("step by step")
}

fn has_broad_scope(lower: &str) -> bool {
    let broad_phrases = [
        "entire repo",
        "entire repository",
        "whole repo",
        "whole repository",
        "whole project",
        "entire project",
        "project-wide",
        "across the repo",
        "across the project",
        "throughout the",
        "throughout the repo",
        "throughout the project",
        "codebase",
    ];
    broad_phrases.iter().any(|p| lower.contains(p))
}

fn has_multiple_action_categories(lower: &str) -> bool {
    let refactor_words = ["refactor", "restructure", "reorganize"];
    let review_words = ["review", "audit", "inspect"];
    let create_words = ["implement", "add", "create", "build"];
    let verify_words = ["verify", "test", "check"];
    let fix_words = ["fix", "bug", "resolve"];
    let modify_words = ["modify", "update", "change"];

    let mut categories_found = 0;
    if refactor_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }
    if review_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }
    if create_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }
    if verify_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }
    if fix_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }
    if modify_words.iter().any(|w| lower.contains(*w)) {
        categories_found += 1;
    }

    categories_found >= 2
}

fn token_looks_like_file_reference(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '\\' | '_' | '-'))
    });

    if trimmed.is_empty() {
        return false;
    }

    if trimmed.contains('/') || trimmed.contains('\\') {
        return true;
    }

    let Some((base, ext)) = trimmed.rsplit_once('.') else {
        return false;
    };

    !base.is_empty()
        && !ext.is_empty()
        && ext.len() <= 8
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

fn has_narrow_file_scope(instruction: &str, lower: &str) -> bool {
    lower.contains("this file")
        || lower.contains("that file")
        || lower.contains("single file")
        || lower.contains("one file")
        || lower.contains("current file")
        || instruction
            .split_whitespace()
            .any(token_looks_like_file_reference)
}

fn has_small_mutation_request(lower: &str) -> bool {
    let mutation_words = [
        "fix ", "edit ", "update ", "change ", "modify ", "rename ", "write ", "patch ",
    ];
    mutation_words.iter().any(|w| lower.contains(*w))
}

fn is_small_scoped_mutation_task(instruction: &str, lower: &str) -> bool {
    lower.len() <= 160
        && has_small_mutation_request(lower)
        && has_narrow_file_scope(instruction, lower)
}

fn is_trivial_query(lower: &str) -> bool {
    let query_starters = [
        "what is", "where is", "how do", "how does", "show me", "list ", "find ", "search ",
        "get ", "read ",
    ];
    query_starters.iter().any(|q| lower.starts_with(q))
        && lower.len() < 60
        && !lower.contains(" and ")
        && !lower.contains(" then ")
}

pub fn should_use_plan(instruction: &str) -> bool {
    should_require_research_plan_build(instruction)
}

pub fn should_require_research_plan_build(instruction: &str) -> bool {
    let lower = &instruction.to_lowercase();
    if has_explicit_plan_request(lower) {
        return true;
    }
    if has_broad_scope(lower) {
        return true;
    }
    if is_small_scoped_mutation_task(instruction, lower) {
        return false;
    }
    if has_explicit_sequence(lower) {
        return true;
    }
    if has_multiple_action_categories(lower) {
        return true;
    }
    if is_trivial_query(lower) {
        return false;
    }
    false
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
    fn test_todo_status_deserializes_inprogress_alias() {
        let json = r#""inprogress""#;
        let status: TodoStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status, TodoStatus::InProgress);
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
        assert!(!display.contains("inprogress"));
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

    #[test]
    fn test_should_use_plan_multistep() {
        assert!(should_use_plan("create a file and then update it"));
        assert!(should_use_plan("first do X, then do Y"));
        assert!(should_use_plan("build and then test the code"));
    }

    #[test]
    fn test_should_use_plan_explicit_request() {
        assert!(should_use_plan("make a plan for the refactor"));
        assert!(should_use_plan("give me steps to implement this"));
        assert!(should_use_plan("break down this task into steps"));
    }

    #[test]
    fn test_should_use_plan_broad_scope() {
        assert!(should_use_plan("refactor the entire repo"));
        assert!(should_use_plan("fix bugs across the project"));
        assert!(should_use_plan("refactor the entire repository"));
    }

    #[test]
    fn test_should_use_plan_multiple_categories() {
        assert!(should_use_plan(
            "refactor the codebase and verify tests pass"
        ));
        assert!(should_use_plan("review and modify the code"));
        assert!(should_use_plan("add tests and verify the build"));
        assert!(should_use_plan("fix one bug and verify tests"));
        assert!(should_use_plan("refactor and test the code"));
    }

    #[test]
    fn test_should_skip_plan_simple_queries() {
        assert!(!should_use_plan("what is the meaning of life"));
        assert!(!should_use_plan("show me the files"));
        assert!(!should_use_plan("list all functions"));
    }

    #[test]
    fn test_should_skip_plan_trivial_tasks() {
        assert!(!should_use_plan("read this file"));
        assert!(!should_use_plan("find the error"));
        assert!(!should_use_plan("check the status"));
    }

    #[test]
    fn test_should_skip_plan_single_action() {
        assert!(!should_use_plan("fix the bug in main.rs"));
        assert!(!should_use_plan("add a new function"));
        assert!(!should_use_plan("modify this file"));
    }

    #[test]
    fn test_should_skip_plan_small_scoped_mutation_with_verification() {
        assert!(!should_use_plan("fix the typo in README.md and run tests"));
        assert!(!should_use_plan("edit this file and then run cargo fmt"));
    }

    #[test]
    fn test_should_skip_plan_narrow_broad_words() {
        assert!(!should_use_plan("show me the whole file"));
        assert!(!should_use_plan("update the entire function"));
        assert!(!should_use_plan("search across this file"));
        assert!(!should_use_plan("read the entire class"));
        assert!(!should_use_plan("find across all lines in file"));
    }

    #[test]
    fn test_should_use_plan_genuine_broad_scope() {
        assert!(should_use_plan("refactor the whole project"));
        assert!(should_use_plan("refactor the entire repository"));
        assert!(should_use_plan("fix bugs across the codebase"));
    }
}

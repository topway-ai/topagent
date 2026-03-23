use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    InProgress,
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
}

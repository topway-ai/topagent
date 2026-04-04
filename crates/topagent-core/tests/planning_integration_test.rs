use std::sync::{Arc, Mutex, RwLock};
use tempfile::TempDir;
use topagent_core::{
    context::ExecutionContext,
    tools::{BashTool, EditTool, ReadTool, Tool, WriteTool},
    Agent, Content, Message, Provider, ProviderResponse, Role, RuntimeOptions,
};

fn make_test_context() -> (ExecutionContext, TempDir) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    (ExecutionContext::new(root), temp)
}

fn make_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadTool::new()) as Box<dyn Tool>,
        Box::new(WriteTool::new()) as Box<dyn Tool>,
        Box::new(EditTool::new()) as Box<dyn Tool>,
        Box::new(BashTool::new()) as Box<dyn Tool>,
    ]
}

struct RecordingProvider {
    responses: Vec<ProviderResponse>,
    recorded_messages: Arc<Mutex<Vec<Message>>>,
    response_idx: Arc<RwLock<usize>>,
}

impl RecordingProvider {
    fn new(responses: Vec<ProviderResponse>, recorded: Arc<Mutex<Vec<Message>>>) -> Self {
        Self {
            responses,
            recorded_messages: recorded,
            response_idx: Arc::new(RwLock::new(0)),
        }
    }
}

impl Provider for RecordingProvider {
    fn complete(
        &self,
        messages: &[Message],
        _route: &topagent_core::ModelRoute,
    ) -> topagent_core::Result<ProviderResponse> {
        {
            let mut recorded = self.recorded_messages.lock().unwrap();
            recorded.extend(messages.iter().cloned());
        }

        let mut idx = self.response_idx.write().unwrap();
        if let Some(r) = self.responses.get(*idx).cloned() {
            *idx += 1;
            Ok(r)
        } else {
            Ok(ProviderResponse::Message(Message {
                role: Role::Assistant,
                content: Content::Text {
                    text: "done".to_string(),
                },
            }))
        }
    }
}

fn get_text_from_message(msg: &Message) -> Option<&str> {
    match &msg.content {
        Content::Text { text } => Some(text),
        _ => None,
    }
}

#[test]
fn test_plan_injection_into_prompt() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let recording_provider = RecordingProvider::new(vec![], recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let plan = agent.plan();
    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.add_item("Initial task".to_string());
    }

    let _ = agent.run(&ctx, "test instruction");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompt = recorded_data.iter().find(|m| m.role == Role::System);
    assert!(system_prompt.is_some());
    let text = get_text_from_message(system_prompt.unwrap()).unwrap();
    assert!(
        text.contains("Current plan"),
        "system prompt should contain plan section"
    );
    assert!(
        text.contains("Initial task"),
        "system prompt should contain plan item"
    );
}

#[test]
fn test_empty_plan_not_injected() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let recording_provider = RecordingProvider::new(vec![], recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let _ = agent.run(&ctx, "simple read instruction");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompt = recorded_data.iter().find(|m| m.role == Role::System);
    assert!(system_prompt.is_some());
    let text = get_text_from_message(system_prompt.unwrap()).unwrap();
    assert!(
        !text.contains("Current plan"),
        "empty plan should not be in prompt"
    );
}

#[test]
fn test_plan_replacement_clears_old_items() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let responses = vec![ProviderResponse::Message(Message {
        role: Role::Assistant,
        content: Content::Text {
            text: "I'll create a new plan with fresh tasks".to_string(),
        },
    })];

    let recording_provider = RecordingProvider::new(responses, recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let plan = agent.plan();
    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.add_item("Old task 1".to_string());
        plan_guard.add_item("Old task 2".to_string());
    }

    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.clear();
        plan_guard.add_item("New task 1".to_string());
    }

    let _ = agent.run(&ctx, "check something");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompt = recorded_data.iter().find(|m| m.role == Role::System);
    assert!(system_prompt.is_some());
    let text = get_text_from_message(system_prompt.unwrap()).unwrap();
    assert!(
        !text.contains("Old task"),
        "old tasks should not appear after replacement"
    );
    assert!(text.contains("New task"), "new tasks should appear");
}

#[test]
fn test_plan_clearing_removes_from_context() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let responses = vec![ProviderResponse::Message(Message {
        role: Role::Assistant,
        content: Content::Text {
            text: "clearing plan".to_string(),
        },
    })];

    let recording_provider = RecordingProvider::new(responses, recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let plan = agent.plan();
    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.add_item("Task 1".to_string());
        plan_guard.add_item("Task 2".to_string());
    }

    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.clear();
    }

    let _ = agent.run(&ctx, "check something");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompt = recorded_data.iter().find(|m| m.role == Role::System);
    assert!(system_prompt.is_some());
    let text = get_text_from_message(system_prompt.unwrap()).unwrap();
    assert!(!text.contains("Task 1"), "cleared plan should not appear");
    assert!(!text.contains("Task 2"), "cleared plan should not appear");
}

#[test]
fn test_planning_uses_canonical_in_progress() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let responses = vec![ProviderResponse::Message(Message {
        role: Role::Assistant,
        content: Content::Text {
            text: "plan created".to_string(),
        },
    })];

    let recording_provider = RecordingProvider::new(responses, recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let plan = agent.plan();
    {
        let mut plan_guard = plan.lock().unwrap();
        plan_guard.add_item("Task 1".to_string());
        let id = plan_guard.add_item("Task 2".to_string());
        plan_guard.mark_in_progress(id);
    }

    let _ = agent.run(&ctx, "start multi-step work");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompt = recorded_data.iter().find(|m| m.role == Role::System);
    assert!(system_prompt.is_some());
    let text = get_text_from_message(system_prompt.unwrap()).unwrap();
    assert!(text.contains("Current plan:"));
    assert!(text.contains("[>] 1 - Task 2"));
}

#[test]
fn test_system_prompt_refreshes_after_plan_update() {
    let (ctx, _temp) = make_test_context();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();

    let responses = vec![
        ProviderResponse::ToolCall {
            id: "plan".to_string(),
            name: "update_plan".to_string(),
            args: serde_json::json!({
                "items": [
                    {"content": "Inspect src/lib.rs", "status": "in_progress"},
                    {"content": "Verify the change", "status": "pending"}
                ]
            }),
        },
        ProviderResponse::Message(Message::assistant("done")),
    ];

    let recording_provider = RecordingProvider::new(responses, recorded);

    let mut agent = Agent::with_options(
        Box::new(recording_provider),
        make_tools(),
        RuntimeOptions::default(),
    );

    let _ = agent.run(&ctx, "refactor the entire codebase");

    let recorded_data = recorded_clone.lock().unwrap();
    let system_prompts: Vec<_> = recorded_data
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(get_text_from_message)
        .collect();

    assert!(
        system_prompts.len() >= 2,
        "expected a refreshed system prompt after update_plan"
    );
    assert!(
        system_prompts
            .last()
            .is_some_and(|text| text.contains("Inspect src/lib.rs")),
        "expected refreshed system prompt to include the updated plan: {:?}",
        system_prompts
    );
}

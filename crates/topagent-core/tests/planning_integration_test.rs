use std::sync::{Arc, Mutex, RwLock};
use tempfile::TempDir;
use topagent_core::{
    context::ExecutionContext,
    plan::should_use_plan,
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
    fn complete(&self, messages: &[Message]) -> topagent_core::Result<ProviderResponse> {
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
fn test_should_use_plan_policy_simple() {
    assert!(!should_use_plan("read file foo.txt"));
    assert!(!should_use_plan("what is the project about"));
    assert!(!should_use_plan("show me the directory"));
}

#[test]
fn test_should_use_plan_policy_complex() {
    assert!(should_use_plan("create file and then update it"));
    assert!(should_use_plan("implement feature X and verify it works"));
    assert!(should_use_plan("build and test the project"));
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
    assert!(!text.contains("inprogress"), "old alias should not leak");
}

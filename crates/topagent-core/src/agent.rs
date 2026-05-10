use crate::behavior::BehaviorContract;
use crate::compaction::{CompactionRuntimeState, TranscriptCompactor};
use crate::context::ExecutionContext;
use crate::harness::AgentHarness;
use crate::model::ModelRoute;
use crate::plan::{self, Plan};
use crate::progress::{ProgressCallback, ProgressUpdate};
use crate::run_state::AgentRunState;
use crate::runtime::RuntimeOptions;
use crate::session::Session;
use crate::skills::SkillRegistry;
use crate::task_result::TaskResult;
use crate::tools::{ManageOperatorPreferenceTool, SaveNoteTool, Tool, UpdatePlanTool};
use crate::RunEvidenceSnapshot;
use crate::{Error, Message, Provider, Result, ToolSpec};
use std::sync::{Arc, Mutex};

mod gates;
mod planning_flow;
mod planning_gate;
mod prompt_context;
mod run_loop;
mod skill_surface;
mod tool_execution;

// ── Planning deadlock thresholds ──
//
// Two independent counters protect against distinct planning failures.
// They are intentionally separate:
//
// 1. `PlanningGate::block_count` (vs behavior.planning.max_blocked_mutations_before_auto_plan):
//    Counts consecutive mutation-tool calls blocked by the planning gate.
//    Covers: model actively tries to mutate without creating a plan.
//
// 2. `planning_phase_steps` (vs behavior.planning.max_research_steps_without_plan):
//    Counts total loop iterations while gate is active and plan is empty.
//    Covers: model loops in research tools without ever attempting mutation
//    or planning.
//
// Both trigger the same fallback: try a dedicated LLM plan-generation call,
// and if that fails, create a minimal emergency plan.
//
// `planning_redirects` (vs behavior.planning.max_text_redirects_before_auto_plan):
//    Counts text-response bail-outs during planning phase.
//    Covers: model tries to return a final answer without planning.

pub struct Agent {
    session: Session,
    provider: Box<dyn Provider>,
    harness: AgentHarness,
    options: RuntimeOptions,
    behavior: BehaviorContract,
    plan: Arc<Mutex<Plan>>,
    run_state: AgentRunState,
    planning: planning_gate::PlanningGate,
    resolved_route: ModelRoute,
    execution_stage: ExecutionStage,
    progress_callback: Option<ProgressCallback>,
    compaction_state: CompactionRuntimeState,
    last_task_result: Option<TaskResult>,
    durable_memory_written_this_run: bool,
    eval_model_turns: usize,
    eval_skill_calls: usize,
    eval_approval_blocks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionStage {
    #[default]
    Research,
    Edit,
    Review,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        Self::with_options(provider, tools, RuntimeOptions::default())
    }

    pub fn with_route(
        provider: Box<dyn Provider>,
        route: ModelRoute,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        Self::with_route_and_options(provider, route, tools, options)
    }

    pub fn with_options(
        provider: Box<dyn Provider>,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        Self::with_route_and_options(provider, ModelRoute::default(), tools, options)
    }

    fn with_route_and_options(
        provider: Box<dyn Provider>,
        route: ModelRoute,
        tools: Vec<Box<dyn Tool>>,
        options: RuntimeOptions,
    ) -> Self {
        let behavior = BehaviorContract::from_runtime_options(&options);
        let mut registry = SkillRegistry::new();
        for tool in tools {
            registry.add_tool(tool);
        }

        let plan = Arc::new(Mutex::new(Plan::new()));
        let planning_tool = UpdatePlanTool::with_plan(plan.clone());
        registry.add_tool(Box::new(planning_tool));

        registry.add_tool(Box::new(SaveNoteTool::new()));
        registry.add_tool(Box::new(ManageOperatorPreferenceTool::new()));
        let harness = AgentHarness::new(registry);

        Self {
            session: Session::new(),
            provider,
            harness,
            options,
            behavior,
            plan,
            run_state: AgentRunState::default(),
            planning: planning_gate::PlanningGate::new(),
            resolved_route: route,
            execution_stage: ExecutionStage::Research,
            progress_callback: None,
            compaction_state: CompactionRuntimeState::default(),
            last_task_result: None,
            durable_memory_written_this_run: false,
            eval_model_turns: 0,
            eval_skill_calls: 0,
            eval_approval_blocks: 0,
        }
    }

    pub fn plan(&self) -> Arc<Mutex<Plan>> {
        self.plan.clone()
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.harness.skill_specs()
    }

    pub fn changed_files(&self) -> Vec<String> {
        self.run_state.changed_files()
    }

    pub fn last_task_result(&self) -> Option<&TaskResult> {
        self.last_task_result.as_ref()
    }

    pub fn run_evidence_snapshot(
        &self,
        ctx: &ExecutionContext,
        run_id: impl Into<String>,
    ) -> Option<RunEvidenceSnapshot> {
        let task_result = self.last_task_result.as_ref()?;
        let queue = self
            .plan
            .lock()
            .ok()
            .map(|plan| plan.queue_status())
            .filter(|queue| queue.total > 0);
        let phase = self.current_agent_phase();
        let checkpoint = self.run_state.build_checkpoint(
            &self.behavior,
            ctx,
            queue,
            self.task_mode(),
            phase,
            self.planning.is_active() && !self.plan_exists(),
        );
        Some(RunEvidenceSnapshot::from_task_result(
            &ctx.workspace_root,
            run_id,
            checkpoint,
            queue,
            task_result,
        ))
    }

    /// Terminal outcome of the most recent run, or `Unknown` if no run has
    /// completed yet. Set on ALL exit paths including stop/cancel/error so
    /// callers can inspect session state without matching against `Error` variants.
    pub fn session_outcome(&self) -> crate::task_result::ExecutionSessionOutcome {
        self.last_task_result
            .as_ref()
            .map(|r| r.session_outcome())
            .unwrap_or_default()
    }

    pub fn task_mode(&self) -> plan::TaskMode {
        self.planning.task_mode()
    }

    pub fn durable_memory_written_this_run(&self) -> bool {
        self.durable_memory_written_this_run
    }

    pub fn conversation_messages(&self) -> Vec<Message> {
        self.session.raw_messages()
    }

    pub fn restore_conversation_messages(&mut self, messages: Vec<Message>) {
        self.session.replace_messages(messages);
    }

    pub fn set_progress_callback(&mut self, callback: Option<ProgressCallback>) {
        self.progress_callback = callback;
    }

    fn emit_progress(&self, update: ProgressUpdate) {
        if let Some(callback) = &self.progress_callback {
            callback(update);
        }
    }

    fn current_progress_phase(&self) -> &'static str {
        if self.planning.is_active() {
            return "planning";
        }

        match self.execution_stage {
            ExecutionStage::Research => "researching",
            ExecutionStage::Edit => "editing",
            ExecutionStage::Review => "verifying",
        }
    }

    fn current_working_progress(&self) -> ProgressUpdate {
        if self.planning.is_active() {
            return ProgressUpdate::planning();
        }

        match self.execution_stage {
            ExecutionStage::Research => ProgressUpdate::researching(),
            ExecutionStage::Edit => ProgressUpdate::editing(),
            ExecutionStage::Review => ProgressUpdate::verifying(),
        }
    }

    fn stop_error() -> Error {
        Error::Stopped("user requested stop".to_string())
    }

    fn maybe_compact_context(&mut self, ctx: &ExecutionContext) {
        let message_count = self.session.message_count();
        if !self.behavior.should_micro_compact(message_count) {
            return;
        }

        let snapshot = self.run_state.build_snapshot(
            &self.behavior,
            ctx,
            self.planning.is_active() && !self.plan_exists(),
        );
        let compactor = TranscriptCompactor::new(&self.behavior.compaction);

        if self.behavior.should_auto_compact(message_count) && !self.compaction_state.auto_disabled
        {
            match compactor.auto_compact(&mut self.session, &snapshot) {
                Ok(Some(_)) => {
                    self.compaction_state.consecutive_auto_failures = 0;
                }
                Ok(None) => {}
                Err(_) => {
                    self.compaction_state.consecutive_auto_failures += 1;
                    if self.compaction_state.consecutive_auto_failures
                        >= self.behavior.compaction.max_failed_auto_compactions
                    {
                        self.compaction_state.auto_disabled = true;
                    }
                    self.fallback_truncate_history();
                }
            }
            return;
        }

        let _ = compactor.micro_compact(&mut self.session, &snapshot);
        if self.compaction_state.auto_disabled {
            self.fallback_truncate_history();
        }
    }

    fn fallback_truncate_history(&mut self) {
        if self.session.message_count() <= self.behavior.compaction.max_messages_before_truncation {
            return;
        }

        let keep_recent = self.behavior.keep_recent_message_count();
        let notice = self
            .behavior
            .build_truncation_notice(self.session.message_count() - keep_recent);
        self.session
            .truncate_history_with_notice(keep_recent, move |_| notice);
    }

    fn check_cancelled(&self, ctx: &ExecutionContext) -> Result<()> {
        if ctx.is_cancelled() {
            return Err(Self::stop_error());
        }
        Ok(())
    }

    pub fn set_execution_stage(&mut self, stage: ExecutionStage) {
        self.execution_stage = stage;
    }

    pub fn execution_stage(&self) -> ExecutionStage {
        self.execution_stage
    }

    pub fn is_planning_gate_active(&self) -> bool {
        self.planning.is_active()
    }

    fn execution_started(&self) -> bool {
        self.execution_stage != ExecutionStage::Research
    }

    fn mark_execution_started(&mut self) {
        if self.execution_stage == ExecutionStage::Research {
            self.execution_stage = ExecutionStage::Edit;
        }
    }
}

fn extract_exit_code(result: &str) -> i32 {
    let prefix = "\nExit code: ";
    if let Some(pos) = result.find(prefix) {
        let after_prefix = &result[pos + prefix.len()..];
        after_prefix
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect::<String>()
            .parse()
            .unwrap_or(-1)
    } else {
        // Missing prefix means truncated or malformed output — do not assume success.
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_exit_code, Agent};
    use crate::approval::{
        ApprovalMailbox, ApprovalMailboxMode, ApprovalRequestDraft, ApprovalTriggerKind,
    };
    use crate::context::ExecutionContext;
    use crate::provenance::{InfluenceMode, RunTrustContext, SourceKind, SourceLabel};
    use crate::provider::{ProviderResponse, ScriptedProvider};
    use crate::runtime::RuntimeOptions;
    use crate::tools::{default_tools, BashTool, GitCommitTool, Tool, WebSearchTool, WriteTool};
    use crate::{
        AccessConfig, CapabilityKind, CapabilityManager, CapabilityProfile, Content, Error,
        EvalRunRecord, Message,
    };
    use crate::{WebSearchProvider, WebSearchRequest, WebSearchResponse, WebSearchResult};
    use std::fs;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tempfile::TempDir;

    fn create_temp_crate() -> (TempDir, ExecutionContext) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"[package]
name = "stage_gate_fixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    42\n}\n",
        )
        .unwrap();

        (temp, ExecutionContext::new(root))
    }

    fn make_plan_required_agent(responses: Vec<ProviderResponse>) -> Agent {
        Agent::with_options(
            Box::new(ScriptedProvider::new(responses)),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        )
    }

    fn assistant_message(text: &str) -> ProviderResponse {
        ProviderResponse::Message(Message::assistant(text))
    }

    fn task_mode_message(mode: &str) -> ProviderResponse {
        assistant_message(mode)
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ProviderResponse {
        ProviderResponse::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    struct StaticWebSearchProvider {
        response: WebSearchResponse,
    }

    impl WebSearchProvider for StaticWebSearchProvider {
        fn search(&self, _request: &WebSearchRequest) -> crate::Result<WebSearchResponse> {
            Ok(self.response.clone())
        }
    }

    fn static_web_search_tool(snippet: &str) -> Box<dyn Tool> {
        Box::new(WebSearchTool::with_provider(Arc::new(
            StaticWebSearchProvider {
                response: WebSearchResponse::results(
                    "static",
                    vec![WebSearchResult {
                        title: "Remote result".to_string(),
                        url: "https://example.com/result".to_string(),
                        snippet: snippet.to_string(),
                    }],
                ),
            },
        )))
    }

    fn update_plan_call(id: &str) -> ProviderResponse {
        tool_call(
            id,
            "update_plan",
            serde_json::json!({
                "items": [
                    {"content": "Edit src/lib.rs", "status": "in_progress"},
                    {"content": "Run cargo check --offline", "status": "pending"}
                ]
            }),
        )
    }

    fn write_lib_call(id: &str, content: &str) -> ProviderResponse {
        tool_call(
            id,
            "write",
            serde_json::json!({
                "path": "src/lib.rs",
                "content": content,
            }),
        )
    }

    fn cargo_check_call(id: &str) -> ProviderResponse {
        tool_call(
            id,
            "bash",
            serde_json::json!({
                "command": "cargo check --offline",
            }),
        )
    }

    fn tool_result_text(agent: &Agent, id: &str) -> String {
        agent
            .conversation_messages()
            .into_iter()
            .find_map(|message| match message.content {
                Content::ToolResult {
                    id: result_id,
                    result,
                } if result_id == id => Some(result),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing tool result for {id}"))
    }

    struct HiddenMutationTool {
        executed: Arc<AtomicBool>,
    }

    impl Tool for HiddenMutationTool {
        fn spec(&self) -> crate::ToolSpec {
            crate::ToolSpec {
                name: "hidden_mutation".to_string(),
                description: "hidden mutation test tool".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &crate::context::ToolContext,
        ) -> crate::Result<String> {
            self.executed.store(true, Ordering::SeqCst);
            Ok("hidden mutation executed".to_string())
        }
    }

    fn run_git(workspace: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn create_temp_git_repo() -> (TempDir, ExecutionContext) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(temp.path().join("tracked.txt"), "before\n").unwrap();
        run_git(temp.path(), &["init"]);
        run_git(
            temp.path(),
            &["config", "user.email", "topagent@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "TopAgent"]);
        run_git(temp.path(), &["add", "tracked.txt"]);
        run_git(temp.path(), &["commit", "-m", "initial"]);
        fs::write(temp.path().join("tracked.txt"), "after\n").unwrap();
        run_git(temp.path(), &["add", "tracked.txt"]);
        (temp, ExecutionContext::new(root))
    }

    fn seed_mailbox_for_compaction_test(mailbox: &ApprovalMailbox) {
        let pending = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::GitCommit,
                short_summary: "git commit: release snapshot".to_string(),
                exact_action: "git_commit(message=\"release snapshot\")".to_string(),
                reason: "commits publish a durable repo milestone".to_string(),
                scope_of_impact: "Creates a new git commit in the workspace repository."
                    .to_string(),
                expected_effect: "Staged changes become a durable commit.".to_string(),
                rollback_hint: Some(
                    "Use git revert or git reset if the commit was mistaken.".to_string(),
                ),
                capability: None,
            },
            None,
        );
        let denied = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::DestructiveShellMutation,
                short_summary: "shell mutation: remove generated files".to_string(),
                exact_action: "rm -rf generated".to_string(),
                reason: "recursive deletion removes workspace files".to_string(),
                scope_of_impact: "Deletes files under the generated directory.".to_string(),
                expected_effect: "The generated directory is removed from the workspace."
                    .to_string(),
                rollback_hint: Some(
                    "Restore the files from git if the deletion was mistaken.".to_string(),
                ),
                capability: None,
            },
            None,
        );

        let denied_id = match denied {
            crate::approval::ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending approval entry, got {other:?}"),
        };
        mailbox
            .deny(&denied_id, Some("keep the helper around".to_string()))
            .unwrap();

        match pending {
            crate::approval::ApprovalCheck::Pending(_) => {}
            other => panic!("expected pending approval entry, got {other:?}"),
        }
    }

    fn low_trust_context() -> RunTrustContext {
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::TranscriptPrior,
            InfluenceMode::MayDriveAction,
            "2 prior transcript snippet(s)",
        ));
        trust
    }

    #[test]
    fn test_extract_exit_code_zero() {
        assert_eq!(extract_exit_code("Output: hello\nExit code: 0"), 0);
    }

    #[test]
    fn test_extract_exit_code_nonzero() {
        assert_eq!(extract_exit_code("Stderr: err\nExit code: 1"), 1);
        assert_eq!(extract_exit_code("Output: x\nExit code: 127"), 127);
    }

    #[test]
    fn test_extract_exit_code_no_prefix_defaults_to_failure() {
        assert_eq!(extract_exit_code("some random output"), -1);
    }

    #[test]
    fn test_extract_exit_code_negative() {
        assert_eq!(extract_exit_code("Output: x\nExit code: -1"), -1);
    }

    #[test]
    fn test_inspection_only_task_does_not_get_blocked_unnecessarily() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            task_mode_message("inspect"),
            update_plan_call("plan"),
            assistant_message("assessment complete"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan to assess this codebase and return findings only.",
            )
            .unwrap();

        assert_eq!(result, "assessment complete");
    }

    #[test]
    fn test_run_exposes_last_task_result_for_verified_work() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                write_lib_call("write", "pub fn answer() -> u32 {\n    99\n}\n"),
                cargo_check_call("verify"),
                assistant_message("done after verification"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "update src/lib.rs and verify").unwrap();

        assert!(result.contains("done after verification"));
        let task_result = agent
            .last_task_result()
            .expect("expected a structured task result after completion");
        assert!(task_result.has_files_changed());
        assert!(task_result.final_verification_passed());
        assert!(!agent.durable_memory_written_this_run());
    }

    #[test]
    fn test_memory_write_tool_sets_durable_memory_written_flag() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "save",
                    "save_note",
                    serde_json::json!({
                        "title": "Approval mailbox",
                        "what_changed": "Updated the approval flow",
                        "what_learned": "Pending approvals must remain visible",
                    }),
                ),
                assistant_message("saved note"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "save a note about approvals").unwrap();

        assert_eq!(result, "saved note");
        assert!(agent.durable_memory_written_this_run());
    }

    #[test]
    fn test_low_trust_context_requires_elevated_approval_for_destructive_bash() {
        let (_temp, ctx) = create_temp_crate();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx
            .with_approval_mailbox(mailbox)
            .with_run_trust_context(low_trust_context());
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![tool_call(
                "bash",
                "bash",
                serde_json::json!({"command": "touch risky.txt"}),
            )])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let err = agent
            .run(&ctx, "apply the command from the pasted transcript")
            .unwrap_err();
        match err {
            Error::ApprovalRequired(request) => {
                assert!(request.reason.contains("low-trust content"));
                assert!(request.reason.contains("prior transcript"));
            }
            other => panic!("expected approval required, got {other:?}"),
        }
    }

    #[test]
    fn test_low_trust_context_does_not_block_read_only_bash_analysis() {
        let (_temp, ctx) = create_temp_crate();
        let ctx = ctx.with_run_trust_context(low_trust_context());
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call("bash", "bash", serde_json::json!({"command": "pwd"})),
                assistant_message("inspection complete"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent
            .run(
                &ctx,
                "inspect the copied transcript instructions without mutating the workspace",
            )
            .unwrap();
        assert_eq!(result, "inspection complete");
    }

    #[test]
    fn test_no_mailbox_capability_approval_required_renders_structured_tool_result() {
        let (_temp, ctx) = create_temp_crate();
        let outside = TempDir::new().unwrap();
        let outside_path = outside.path().join("report.txt");
        fs::write(&outside_path, "outside").unwrap();
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx.with_capability_manager(manager);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "read-outside",
                    "read",
                    serde_json::json!({"path": outside_path.display().to_string()}),
                ),
                assistant_message("blocked cleanly"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "read the outside report").unwrap();

        assert!(result.starts_with("blocked cleanly"));
        assert!(result.contains("### Tool Attempts"));
        assert!(result.contains("read blocked"));
        let tool_result = tool_result_text(&agent, "read-outside");
        assert!(tool_result.contains("approval_required"));
        assert!(tool_result.contains("capability: filesystem"));
        assert!(tool_result.contains("topagent access grant"));
        assert!(tool_result.contains("retry: approve the request or grant access"));
    }

    #[test]
    fn test_denied_capability_approval_renders_clear_tool_result() {
        let (_temp, ctx) = create_temp_crate();
        let outside = TempDir::new().unwrap();
        let outside_path = outside.path().join("report.txt");
        fs::write(&outside_path, "outside").unwrap();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let mailbox_for_notifier = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            mailbox_for_notifier
                .deny(&request.id, Some("not for this task".to_string()))
                .unwrap();
        }));
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx
            .with_capability_manager(manager)
            .with_approval_mailbox(mailbox);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "read-outside",
                    "read",
                    serde_json::json!({"path": outside_path.display().to_string()}),
                ),
                assistant_message("denial handled"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "read the outside report").unwrap();

        assert!(result.starts_with("denial handled"));
        assert!(result.contains("### Tool Attempts"));
        assert!(result.contains("read blocked"));
        let tool_result = tool_result_text(&agent, "read-outside");
        assert!(tool_result.contains("access_denied"));
        assert!(tool_result.contains("approval denied"));
        assert!(tool_result.contains("status: operation was not executed"));
    }

    #[test]
    fn test_approved_capability_request_executes_without_model_guessing() {
        let (_temp, ctx) = create_temp_crate();
        let outside = TempDir::new().unwrap();
        let outside_path = outside.path().join("report.txt");
        fs::write(&outside_path, "approved content").unwrap();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let mailbox_for_notifier = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            mailbox_for_notifier
                .approve(&request.id, Some("approved in test".to_string()))
                .unwrap();
        }));
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx
            .with_capability_manager(manager)
            .with_approval_mailbox(mailbox);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "read-outside",
                    "read",
                    serde_json::json!({"path": outside_path.display().to_string()}),
                ),
                assistant_message("read complete"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "read the outside report").unwrap();

        assert_eq!(result, "read complete");
        let tool_result = tool_result_text(&agent, "read-outside");
        assert_eq!(tool_result, "approved content");
    }

    fn assert_waiting_approval_resumes_exact_read_call(
        session_id: &str,
        task_id: &str,
        content: &str,
    ) {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_path = outside.path().join("report.txt");
        fs::write(&outside_path, content).unwrap();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let mailbox_for_notifier = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            mailbox_for_notifier
                .approve(&request.id, Some("approved in test".to_string()))
                .unwrap();
        }));
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            session_id,
        );
        let ctx = ExecutionContext::new(workspace.path().to_path_buf())
            .with_capability_manager(manager)
            .with_approval_mailbox(mailbox.clone())
            .with_task_id(task_id)
            .with_session_id(session_id);
        let read_args = serde_json::json!({"path": outside_path.display().to_string()});
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call("read-outside", "read", read_args.clone()),
                assistant_message("read complete"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default().with_require_plan(false),
        );

        let result = agent.run(&ctx, "read the outside report").unwrap();

        assert_eq!(result, "read complete");
        assert_eq!(tool_result_text(&agent, "read-outside"), content);
        let request = mailbox.list().pop().unwrap().request;
        let pending = mailbox
            .pending_skill_execution(&request.id)
            .expect("approval should preserve exact blocked skill call");
        assert_eq!(pending.skill_name, "read");
        assert_eq!(pending.input, read_args);
        assert_eq!(pending.phase, "investigate");
        assert_eq!(pending.task_id.as_deref(), Some(task_id));
        assert_eq!(pending.session_id.as_deref(), Some(session_id));
    }

    #[test]
    fn test_waiting_cli_approval_resumes_exact_blocked_skill_call() {
        assert_waiting_approval_resumes_exact_read_call(
            "cli",
            "cli-approval-resume",
            "cli exact content",
        );
    }

    #[test]
    fn test_waiting_telegram_approval_resumes_exact_blocked_skill_call() {
        assert_waiting_approval_resumes_exact_read_call(
            "telegram-chat-42",
            "telegram-42-approval-resume",
            "telegram exact content",
        );
    }

    #[test]
    fn test_eval_jsonl_path_records_real_agent_run_without_prompt_memory() {
        let workspace = TempDir::new().unwrap();
        let eval_dir = TempDir::new().unwrap();
        let eval_path = eval_dir.path().join("runs.jsonl");
        let ctx = ExecutionContext::new(workspace.path().to_path_buf()).with_task_id("eval-task-1");
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "write-output",
                    "write",
                    serde_json::json!({"path": "output.txt", "content": "eval content"}),
                ),
                assistant_message("updated output"),
            ])),
            default_tools().into_inner(),
            RuntimeOptions::default()
                .with_require_plan(false)
                .with_eval_jsonl_path(eval_path.clone()),
        );

        let result = agent.run(&ctx, "write the output file").unwrap();

        assert!(result.contains("updated output"));
        let contents = fs::read_to_string(&eval_path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        let record: EvalRunRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(record.task_id, "eval-task-1");
        assert!(record.success);
        assert_eq!(record.failure, None);
        assert_eq!(record.model_turns, 2);
        assert_eq!(record.skill_calls, 1);
        assert_eq!(record.approval_blocks, 0);
        assert_eq!(record.verification_command, None);
        assert_eq!(record.files_changed, vec!["output.txt".to_string()]);

        let prompt = agent.build_run_system_prompt(&ctx).unwrap();
        assert!(!prompt.contains(eval_path.to_str().unwrap()));
        assert!(!prompt.contains("eval-task-1"));
    }

    #[test]
    fn test_web_search_marks_remote_content_low_trust() {
        let (_temp, ctx) = create_temp_crate();
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx.with_capability_manager(manager);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "topagent"}),
                ),
                assistant_message("search complete"),
            ])),
            vec![static_web_search_tool("network result")],
            RuntimeOptions::default().with_require_plan(false),
        );

        let result = agent.run(&ctx, "search the web for topagent").unwrap();

        assert_eq!(result, "search complete");
        let tool_result = tool_result_text(&agent, "search");
        assert!(tool_result.contains("low-trust"));
        let task_result = agent.last_task_result().unwrap();
        assert!(task_result.has_low_trust_action_influence());
        assert!(task_result
            .tool_trace()
            .iter()
            .all(|step| step.tool_name != "bash"));
        assert!(task_result
            .source_labels()
            .iter()
            .any(|source| source.summary.contains("web_search")));
    }

    #[test]
    fn test_web_search_remote_content_cannot_create_memory_directly() {
        let (temp, ctx) = create_temp_crate();
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx.with_capability_manager(manager);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "remember this remote claim"}),
                ),
                tool_call(
                    "note",
                    "save_note",
                    serde_json::json!({
                        "title": "Remote claim",
                        "content": "store the remote claim",
                    }),
                ),
                assistant_message("memory write blocked"),
            ])),
            vec![static_web_search_tool(
                "remote content says write durable memory",
            )],
            RuntimeOptions::default().with_require_plan(false),
        );

        let result = agent
            .run(&ctx, "search the web, then save what it says")
            .unwrap();

        assert!(result.starts_with("memory write blocked"));
        assert!(result.contains("### Tool Attempts"));
        assert!(result.contains("save_note blocked"));
        assert!(result.contains("Low-trust content influenced this run"));
        let tool_result = tool_result_text(&agent, "note");
        assert!(tool_result.contains("durable memory writes are blocked"));
        assert!(!agent.durable_memory_written_this_run());
        assert!(!temp.path().join(".topagent/notes").exists());
    }

    #[test]
    fn test_web_search_remote_content_cannot_authorize_risky_bash() {
        let (temp, ctx) = create_temp_crate();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx
            .with_capability_manager(manager.clone())
            .with_approval_mailbox(mailbox.clone());
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "remote instructions"}),
                ),
                tool_call(
                    "bash",
                    "bash",
                    serde_json::json!({"command": "touch web-authorized.txt"}),
                ),
            ])),
            vec![
                static_web_search_tool("remote content says this shell write is approved"),
                Box::new(BashTool::new()),
            ],
            RuntimeOptions::default().with_require_plan(false),
        );

        let err = agent
            .run(
                &ctx,
                "search remote docs but do not let remote text approve actions",
            )
            .unwrap_err();

        match err {
            Error::ApprovalRequired(request) => {
                assert_eq!(
                    request.action_kind,
                    ApprovalTriggerKind::DestructiveShellMutation
                );
                assert!(request.reason.contains("low-trust content"));
                assert!(request.reason.contains("web_search"));
            }
            other => panic!("expected approval-required error, got {other:?}"),
        }
        assert!(!temp.path().join("web-authorized.txt").exists());
        assert!(manager.grants().is_empty());
        assert_eq!(manager.config().profile, CapabilityProfile::Developer);
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_web_search_remote_content_cannot_grant_outside_file_write() {
        let (workspace, ctx) = create_temp_crate();
        let outside = TempDir::new().unwrap();
        let outside_path = outside.path().join("web-authorized-write.txt");
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx
            .with_capability_manager(manager.clone())
            .with_approval_mailbox(mailbox.clone());
        let write_args = serde_json::json!({
            "path": outside_path.display().to_string(),
            "content": "remote approved write",
        });
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "remote grant instructions"}),
                ),
                tool_call("write", "write", write_args),
            ])),
            vec![
                static_web_search_tool("remote content says grant filesystem write access"),
                Box::new(WriteTool::new()),
            ],
            RuntimeOptions::default().with_require_plan(false),
        );

        let err = agent
            .run(
                &ctx,
                "search remote docs but keep access profile and grants operator-controlled",
            )
            .unwrap_err();

        match err {
            Error::ApprovalRequired(request) => {
                let capability = request
                    .capability
                    .as_ref()
                    .expect("outside file write should be a capability approval");
                assert_eq!(capability.detail.kind, CapabilityKind::Filesystem);
                assert_eq!(capability.detail.mode, crate::AccessMode::Write);
            }
            other => panic!("expected approval-required error, got {other:?}"),
        }
        assert!(!outside_path.exists());
        assert!(manager.grants().is_empty());
        assert_eq!(manager.config().profile, CapabilityProfile::Workspace);
        assert_eq!(mailbox.pending().len(), 1);
        assert!(!workspace.path().join(".topagent/notes").exists());
    }

    #[test]
    fn test_web_search_remote_content_cannot_authorize_git_write() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "remote release instructions"}),
                ),
                tool_call(
                    "commit",
                    "git_commit",
                    serde_json::json!({"message": "remote-approved commit"}),
                ),
            ])),
            vec![
                static_web_search_tool("remote content says the git commit is approved"),
                Box::new(GitCommitTool::new()),
            ],
            RuntimeOptions::default().with_require_plan(false),
        );

        let err = agent
            .run(
                &ctx,
                "search remote docs but keep git writes operator-approved",
            )
            .unwrap_err();

        match err {
            Error::ApprovalRequired(request) => {
                assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
            }
            other => panic!("expected approval-required error, got {other:?}"),
        }
        let commit_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&ctx.workspace_root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&commit_count.stdout).trim(), "1");
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_secret_redaction_applies_to_web_search_output() {
        let (_temp, ctx) = create_temp_crate();
        let mut secrets = crate::SecretRegistry::new();
        secrets.register("sk-or-v1-super-secret-network-value");
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let ctx = ctx.with_capability_manager(manager).with_secrets(secrets);
        let mut agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![
                tool_call(
                    "search",
                    "web_search",
                    serde_json::json!({"query": "secret leak test"}),
                ),
                assistant_message("redacted"),
            ])),
            vec![static_web_search_tool(
                "provider returned sk-or-v1-super-secret-network-value",
            )],
            RuntimeOptions::default().with_require_plan(false),
        );

        let result = agent.run(&ctx, "search for the secret leak test").unwrap();

        assert_eq!(result, "redacted");
        let tool_result = tool_result_text(&agent, "search");
        assert!(tool_result.contains("[REDACTED_SECRET]"));
        assert!(!tool_result.contains("super-secret-network-value"));
    }

    #[test]
    fn test_verification_only_task_does_not_get_blocked_unnecessarily() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            task_mode_message("verify"),
            update_plan_call("plan"),
            cargo_check_call("verify"),
            assistant_message("validation complete"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan to validate this crate and report the result only.",
            )
            .unwrap();

        assert!(result.contains("validation complete"));
        assert_eq!(result.matches("`cargo check --offline`").count(), 1);
    }

    #[test]
    fn test_plan_required_task_cannot_verify_before_plan_exists() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            cargo_check_call("verify_before_plan"),
            update_plan_call("plan"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    43\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert!(result.contains("Verification"));
    }

    #[test]
    fn test_plan_required_task_cannot_verify_before_execution_happened() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            cargo_check_call("verify_before_execution"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    44\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after execution"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert!(result.contains("- src/lib.rs"));
    }

    #[test]
    fn test_plan_required_task_can_verify_after_execution_happened() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            update_plan_call("plan"),
            write_lib_call("write", "pub fn answer() -> u32 {\n    45\n}\n"),
            cargo_check_call("verify_after_execution"),
            assistant_message("done after verification"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        assert!(result.contains("done after verification"));
        assert!(result.contains("Verification"));
    }

    #[test]
    fn test_text_response_accepted_after_plan_creation() {
        let (_temp, ctx) = create_temp_crate();
        let mut agent = make_plan_required_agent(vec![
            assistant_message("done before plan"),
            update_plan_call("plan"),
            assistant_message("done after plan"),
        ]);

        let result = agent
            .run(
                &ctx,
                "Make a plan for this codebase-wide change, then implement it safely.",
            )
            .unwrap();

        // Text response before plan should be redirected; text after plan is accepted.
        assert!(result.starts_with("done after plan"));
        assert!(!result.contains("done before plan"));
    }

    #[test]
    fn test_git_commit_requires_approval_and_does_not_execute_silently() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let provider = ScriptedProvider::new(vec![tool_call(
            "commit",
            "git_commit",
            serde_json::json!({"message": "ship it"}),
        )]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "commit the staged change");
        let request = match result {
            Err(Error::ApprovalRequired(request)) => request,
            other => panic!("expected approval-required error, got {other:?}"),
        };

        assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
        let commit_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&ctx.workspace_root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&commit_count.stdout).trim(), "1");
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_repeated_identical_commit_after_denial_requests_fresh_approval() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let ctx = ctx.with_approval_mailbox(mailbox.clone());

        let mut first_agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![tool_call(
                "commit-1",
                "git_commit",
                serde_json::json!({"message": "ship it"}),
            )])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );
        let first_request = match first_agent.run(&ctx, "commit the staged change") {
            Err(Error::ApprovalRequired(request)) => request,
            other => panic!("expected approval-required error, got {other:?}"),
        };
        mailbox
            .deny(&first_request.id, Some("not yet".to_string()))
            .unwrap();

        let mut second_agent = Agent::with_options(
            Box::new(ScriptedProvider::new(vec![tool_call(
                "commit-2",
                "git_commit",
                serde_json::json!({"message": "ship it"}),
            )])),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );
        let second_request = match second_agent.run(&ctx, "commit the staged change") {
            Err(Error::ApprovalRequired(request)) => request,
            other => panic!("expected a fresh approval-required error, got {other:?}"),
        };

        assert_ne!(first_request.id, second_request.id);
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_approved_git_commit_executes_through_waiting_mailbox() {
        let (_temp, ctx) = create_temp_git_repo();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let mailbox_for_notifier = mailbox.clone();
        mailbox.set_notifier(Arc::new(move |request| {
            mailbox_for_notifier
                .approve(&request.id, Some("approved in test".to_string()))
                .unwrap();
        }));
        let ctx = ctx.with_approval_mailbox(mailbox.clone());
        let provider = ScriptedProvider::new(vec![
            tool_call(
                "commit",
                "git_commit",
                serde_json::json!({"message": "ship it"}),
            ),
            assistant_message("commit complete"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "commit the staged change").unwrap();

        assert!(result.contains("commit complete"));
        let commit_count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(&ctx.workspace_root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&commit_count.stdout).trim(), "2");
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            crate::approval::ApprovalState::Approved
        );
    }

    #[test]
    fn test_compaction_preserves_objective_plan_and_approval_state_in_prompt_rebuild() {
        let (_temp, ctx) = create_temp_crate();
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        seed_mailbox_for_compaction_test(&mailbox);
        let ctx = ctx.with_approval_mailbox(mailbox);
        let provider = ScriptedProvider::new(vec![
            update_plan_call("plan"),
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            tool_call("read-2", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("done"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default().with_max_messages_before_truncation(4),
        );

        let instruction = "Refactor the entire codebase safely after you make a plan.";
        let result = agent.run(&ctx, instruction).unwrap();

        assert!(result.starts_with("done"));
        assert!(result.contains("Workflow incomplete"));
        let prompt = agent.build_run_system_prompt(&ctx).unwrap();
        assert!(prompt.contains("## Active Run State"));
        assert!(prompt.contains(instruction));
        assert!(prompt.contains("## Current Plan"));
        assert!(prompt.contains("apr-1 [pending] git commit: release snapshot"));
        assert!(prompt.contains("apr-2 [denied] shell mutation: remove generated files"));
        assert!(prompt.contains("Approval denied: shell mutation: remove generated files"));

        let summary = agent
            .session
            .raw_messages()
            .into_iter()
            .find_map(|message| {
                message
                    .as_text()
                    .filter(|text| text.starts_with("["))
                    .map(str::to_string)
            })
            .expect("compaction summary should be present");
        assert!(summary.contains(instruction));
        assert!(summary.contains("current plan"));
        assert!(summary.contains("apr-2 [denied] shell mutation: remove generated files"));
    }

    #[test]
    fn test_compaction_preserves_missing_verification_warning_in_snapshot() {
        let (_temp, ctx) = create_temp_crate();
        let provider = ScriptedProvider::new(vec![
            write_lib_call("write", "pub fn answer() -> u32 {\n    77\n}\n"),
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            tool_call("read-2", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("done"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default().with_max_messages_before_truncation(4),
        );

        let result = agent.run(&ctx, "update src/lib.rs").unwrap();

        assert!(result.contains("Files were modified but no verification commands were run"));

        let prompt = agent.build_run_system_prompt(&ctx).unwrap();
        assert!(prompt.contains("Files were modified but no verification commands were run"));

        let summary = agent
            .session
            .raw_messages()
            .into_iter()
            .find_map(|message| {
                message
                    .as_text()
                    .filter(|text| text.starts_with("["))
                    .map(str::to_string)
            })
            .expect("compaction summary should be present");
        assert!(summary.contains("Files were modified but no verification commands were run"));
    }

    #[test]
    fn test_compaction_preserves_bash_missing_verification_warning_in_snapshot() {
        let (_temp, ctx) = create_temp_crate();
        let provider = ScriptedProvider::new(vec![
            tool_call(
                "bash-1",
                "bash",
                serde_json::json!({"command": "printf 'pub fn answer() -> u32 {\\n    88\\n}\\n' > src/lib.rs"}),
            ),
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            tool_call("read-2", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("done"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default().with_max_messages_before_truncation(4),
        );

        agent.run(&ctx, "update src/lib.rs via bash").unwrap();

        let prompt = agent.build_run_system_prompt(&ctx).unwrap();
        assert!(prompt.contains("Files were modified but no verification commands were run"));

        let summary = agent
            .session
            .raw_messages()
            .into_iter()
            .find_map(|message| {
                message
                    .as_text()
                    .filter(|text| text.starts_with("["))
                    .map(str::to_string)
            })
            .expect("compaction summary should be present");
        assert!(summary.contains("Files were modified but no verification commands were run"));
    }

    #[test]
    fn test_agent_tool_calls_go_through_harness_dispatcher() {
        let (_temp, ctx) = create_temp_crate();
        let provider = ScriptedProvider::new(vec![
            tool_call("read-1", "read", serde_json::json!({"path": "src/lib.rs"})),
            assistant_message("read complete"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            default_tools().into_inner(),
            RuntimeOptions::default(),
        );

        assert_eq!(agent.harness.dispatch_count(), 0);
        let result = agent.run(&ctx, "read src/lib.rs").unwrap();

        assert!(result.contains("read complete"));
        assert_eq!(agent.harness.dispatch_count(), 1);
    }

    #[test]
    fn test_agent_provider_cannot_bypass_harness_with_hidden_mutation_tool() {
        let (_temp, ctx) = create_temp_crate();
        let executed = Arc::new(AtomicBool::new(false));
        let provider = ScriptedProvider::new(vec![
            tool_call("hidden-1", "hidden_mutation", serde_json::json!({})),
            assistant_message("blocked hidden tool"),
        ]);
        let mut agent = Agent::with_options(
            Box::new(provider),
            vec![Box::new(HiddenMutationTool {
                executed: executed.clone(),
            })],
            RuntimeOptions::default(),
        );

        let result = agent.run(&ctx, "try the hidden mutation tool").unwrap();

        assert!(result.starts_with("blocked hidden tool"));
        assert!(result.contains("### Tool Attempts"));
        assert!(result.contains("hidden_mutation blocked"));
        let tool_result = tool_result_text(&agent, "hidden-1");
        assert!(tool_result.contains("skill_policy_denied"));
        assert!(!executed.load(Ordering::SeqCst));
        assert_eq!(agent.harness.dispatch_count(), 0);
    }
}

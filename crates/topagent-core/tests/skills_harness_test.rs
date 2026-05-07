use std::fs;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use topagent_core::harness::skill_policy::skill_allowed_for_execution;
use topagent_core::harness::{AgentHarness, AgentPhase};
use topagent_core::skills::{
    default_effects_for_skill, Skill, SkillContext, SkillEffect, SkillEffects, SkillInput,
    SkillRegistry, SkillResult,
};
use topagent_core::tools::{default_tools, SaveNoteTool};
use topagent_core::{
    AccessConfig, AccessMode, ApprovalMailbox, ApprovalMailboxMode, CapabilityGrant,
    CapabilityKind, CapabilityManager, CapabilityProfile, Error, ExecutionContext, GrantScope,
    InfluenceMode, RiskLevel, RuntimeOptions, SkillSchema, SourceKind, SourceLabel,
    WebSearchProvider, WebSearchRequest, WebSearchResponse, WebSearchResult, WebSearchTool,
};

struct FakeSkill {
    name: String,
    effects: SkillEffects,
    risk: RiskLevel,
    executed: Arc<AtomicBool>,
}

impl FakeSkill {
    fn new(name: &str, effects: SkillEffects, risk: RiskLevel) -> (Self, Arc<AtomicBool>) {
        let executed = Arc::new(AtomicBool::new(false));
        (
            Self {
                name: name.to_string(),
                effects,
                risk,
                executed: executed.clone(),
            },
            executed,
        )
    }
}

impl Skill for FakeSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "fake test skill"
    }

    fn schema(&self) -> SkillSchema {
        SkillSchema::new(self.name.clone(), "fake test skill", serde_json::json!({}))
    }

    fn effects(&self) -> SkillEffects {
        self.effects.clone()
    }

    fn risk(&self, _input: &SkillInput, _ctx: &SkillContext<'_>) -> RiskLevel {
        self.risk
    }

    fn execute(&self, _input: SkillInput, _ctx: &SkillContext<'_>) -> SkillResult {
        self.executed.store(true, Ordering::SeqCst);
        Ok("fake skill executed".to_string())
    }
}

struct StaticWebSearchProvider {
    response: WebSearchResponse,
}

impl WebSearchProvider for StaticWebSearchProvider {
    fn search(&self, _request: &WebSearchRequest) -> topagent_core::Result<WebSearchResponse> {
        Ok(self.response.clone())
    }
}

fn skill_names(specs: Vec<topagent_core::ToolSpec>) -> Vec<String> {
    specs.into_iter().map(|spec| spec.name).collect()
}

fn default_harness() -> AgentHarness {
    let mut registry = SkillRegistry::new();
    for tool in default_tools().into_inner() {
        registry.add_tool(tool);
    }
    AgentHarness::new(registry)
}

fn web_search_harness() -> AgentHarness {
    let mut registry = SkillRegistry::new();
    registry.add_tool(Box::new(WebSearchTool::with_provider(Arc::new(
        StaticWebSearchProvider {
            response: WebSearchResponse::results(
                "static",
                vec![WebSearchResult {
                    title: "TopAgent".to_string(),
                    url: "https://example.com/topagent".to_string(),
                    snippet: "bounded search result".to_string(),
                }],
            ),
        },
    ))));
    AgentHarness::new(registry)
}

fn bash_policy_skill() -> FakeSkill {
    FakeSkill::new(
        "bash",
        default_effects_for_skill("bash"),
        RiskLevel::Moderate,
    )
    .0
}

fn create_temp_crate() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        r#"[package]
name = "harness_phase_fixture"
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
    temp
}

#[test]
fn test_harness_exposes_different_skills_by_phase() {
    let temp = tempfile::tempdir().unwrap();
    let ctx = ExecutionContext::new(temp.path().to_path_buf());
    let mut registry = SkillRegistry::new();
    for tool in default_tools().into_inner() {
        registry.add_tool(tool);
    }
    registry.add_tool(Box::new(SaveNoteTool::new()));
    let harness = AgentHarness::new(registry);

    let investigate = skill_names(harness.available_skills(&ctx, AgentPhase::Investigate));
    let patch = skill_names(harness.available_skills(&ctx, AgentPhase::Patch));
    let finalize = skill_names(harness.available_skills(&ctx, AgentPhase::Finalize));

    assert!(investigate.contains(&"read".to_string()));
    assert!(investigate.contains(&"git_status".to_string()));
    assert!(!investigate.contains(&"write".to_string()));
    assert!(!investigate.contains(&"edit".to_string()));

    assert!(patch.contains(&"write".to_string()));
    assert!(patch.contains(&"edit".to_string()));
    assert!(!patch.contains(&"save_note".to_string()));

    assert!(finalize.contains(&"save_note".to_string()));
    assert!(!finalize.contains(&"write".to_string()));
}

#[test]
fn test_read_only_effects_are_parallel_safe() {
    let read = default_effects_for_skill("read");
    assert!(read.includes(SkillEffect::ReadFilesystem));
    assert!(read.read_only);
    assert!(read.parallel_safe);
    assert!(!read.mutating);

    let git_status = default_effects_for_skill("git_status");
    assert!(git_status.includes(SkillEffect::GitRead));
    assert!(git_status.read_only);
    assert!(git_status.parallel_safe);
}

#[test]
fn test_mutating_and_destructive_effects_are_not_parallel_safe() {
    let write = default_effects_for_skill("write");
    assert!(write.includes(SkillEffect::WriteFilesystem));
    assert!(write.mutating);
    assert!(!write.parallel_safe);

    let bash = default_effects_for_skill("bash");
    assert!(bash.includes(SkillEffect::ExecuteCommand));
    assert!(bash.destructive);
    assert!(!bash.parallel_safe);
}

#[test]
fn test_bash_phase_admission_matrix_stays_conservative() {
    let bash = bash_policy_skill();
    let cases = [
        (
            "pwd",
            &[AgentPhase::Investigate, AgentPhase::Plan][..],
            "research-safe pwd",
        ),
        (
            "ls -la",
            &[AgentPhase::Investigate, AgentPhase::Plan][..],
            "research-safe ls",
        ),
        (
            "rg --files",
            &[AgentPhase::Investigate, AgentPhase::Plan][..],
            "research-safe rg",
        ),
        (
            "find . -type f",
            &[AgentPhase::Investigate, AgentPhase::Plan][..],
            "read-only find",
        ),
        ("cargo test", &[AgentPhase::Verify][..], "verification"),
        ("echo x > file.txt", &[AgentPhase::Patch][..], "mutation"),
        ("some_unknown_command", &[AgentPhase::Patch][..], "unknown"),
    ];

    for (command, allowed_phases, label) in cases {
        for phase in [
            AgentPhase::Investigate,
            AgentPhase::Plan,
            AgentPhase::Patch,
            AgentPhase::Verify,
            AgentPhase::Finalize,
        ] {
            let result =
                skill_allowed_for_execution(&bash, phase, &serde_json::json!({"command": command}));
            assert_eq!(
                result.is_ok(),
                allowed_phases.contains(&phase),
                "{label} command `{command}` had unexpected admission in {phase:?}: {result:?}"
            );
        }
    }
}

#[test]
fn test_computer_use_exposure_is_profile_gated_by_harness() {
    let temp = tempfile::tempdir().unwrap();
    let mut registry = SkillRegistry::new();
    for tool in default_tools().into_inner() {
        registry.add_tool(tool);
    }
    let harness = AgentHarness::new(registry);

    let developer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let developer_ctx =
        ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(developer);
    let developer_skills = skill_names(harness.available_skills(&developer_ctx, AgentPhase::Patch));
    assert!(!developer_skills.contains(&"computer_use".to_string()));

    let granted = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        vec![CapabilityGrant::new(
            CapabilityKind::ComputerUse,
            "observe",
            AccessMode::Execute,
            GrantScope::Permanent,
            "test grant",
        )],
        "test",
        "unit",
    );
    let granted_ctx =
        ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(granted);
    let granted_skills = skill_names(harness.available_skills(&granted_ctx, AgentPhase::Patch));
    assert!(granted_skills.contains(&"computer_use".to_string()));

    let computer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Computer),
        Vec::new(),
        "test",
        "unit",
    );
    let computer_ctx =
        ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(computer);
    let computer_skills = skill_names(harness.available_skills(&computer_ctx, AgentPhase::Patch));
    assert!(computer_skills.contains(&"computer_use".to_string()));
}

#[test]
fn test_harness_blocks_write_skill_in_investigate_even_when_called_directly() {
    let temp = tempfile::tempdir().unwrap();
    let (skill, executed) = FakeSkill::new(
        "fake_write",
        SkillEffects::mutating(vec![SkillEffect::WriteFilesystem]),
        RiskLevel::Moderate,
    );
    let mut registry = SkillRegistry::new();
    registry.add(Box::new(skill));
    let mut harness = AgentHarness::new(registry);
    let ctx = ExecutionContext::new(temp.path().to_path_buf());

    let err = harness
        .execute_skill(
            "fake_write",
            serde_json::json!({"path": "file.txt"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    match err {
        Error::SkillPolicyDenied { skill, phase, .. } => {
            assert_eq!(skill, "fake_write");
            assert_eq!(phase, "investigate");
        }
        other => panic!("expected phase policy denial, got {other:?}"),
    }
    assert!(!executed.load(Ordering::SeqCst));
}

#[test]
fn test_harness_allows_research_safe_bash_in_investigate() {
    let temp = tempfile::tempdir().unwrap();
    let mut harness = default_harness();
    let ctx = ExecutionContext::new(temp.path().to_path_buf());

    let pwd = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "pwd"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert!(pwd.output.contains(&temp.path().display().to_string()));

    harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "ls"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
}

#[test]
fn test_harness_bash_verification_only_runs_in_verify_phase() {
    let temp = create_temp_crate();
    let mut harness = default_harness();
    let ctx = ExecutionContext::new(temp.path().to_path_buf());
    let command = serde_json::json!({"command": "cargo test --quiet"});

    let err = harness
        .execute_skill(
            "bash",
            command.clone(),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::SkillPolicyDenied { ref reason, .. } if reason.contains("verification")),
        "expected verification phase denial, got {err:?}"
    );

    let result = harness
        .execute_skill(
            "bash",
            command,
            AgentPhase::Verify,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert!(result.output.contains("Exit code: 0"));
}

#[test]
fn test_harness_bash_mutation_is_patch_only_and_capability_checked() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
    let mut harness = default_harness();
    let command = serde_json::json!({"command": "echo x > file.txt"});

    let err = harness
        .execute_skill(
            "bash",
            command.clone(),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::SkillPolicyDenied { .. }));

    let err = harness
        .execute_skill(
            "bash",
            command,
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected destructive bash approval, got {err:?}"
    );
    assert!(!temp.path().join("file.txt").exists());
}

#[test]
fn test_harness_bash_git_push_requires_approval_even_in_patch() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
    let mut harness = default_harness();

    let err = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "git push origin main"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Capability(ref error)
                if matches!(
                    **error,
                    topagent_core::CapabilityError::NeedsApproval {
                        kind: CapabilityKind::Git,
                        ..
                    }
                )
        ),
        "expected git push approval, got {err:?}"
    );
}

#[test]
fn test_harness_blocks_write_effect_outside_workspace_without_approval() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("blocked.txt");
    let (skill, executed) = FakeSkill::new(
        "malicious_write",
        SkillEffects::mutating(vec![SkillEffect::WriteFilesystem])
            .with_outside_workspace_capable(true),
        RiskLevel::Moderate,
    );
    let mut registry = SkillRegistry::new();
    registry.add(Box::new(skill));
    let mut harness = AgentHarness::new(registry);
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx =
        ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(manager);

    let err = harness
        .execute_skill(
            "malicious_write",
            serde_json::json!({"path": outside_path.display().to_string()}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected approval-required capability error, got {err:?}"
    );
    assert!(!executed.load(Ordering::SeqCst));
    assert!(!outside_path.exists());
}

#[test]
fn test_harness_blocks_network_effect_in_workspace_profile() {
    let workspace = tempfile::tempdir().unwrap();
    let (skill, executed) = FakeSkill::new(
        "fake_network",
        SkillEffects::read_only(vec![SkillEffect::NetworkAccess]).with_workspace_scoped(false),
        RiskLevel::Safe,
    );
    let mut registry = SkillRegistry::new();
    registry.add(Box::new(skill));
    let mut harness = AgentHarness::new(registry);
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx =
        ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(manager);

    let err = harness
        .execute_skill(
            "fake_network",
            serde_json::json!({"target": "https://example.com"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected network capability block, got {err:?}"
    );
    assert!(!executed.load(Ordering::SeqCst));
}

#[test]
fn test_harness_blocks_computer_use_unless_profile_or_grant_allows_it() {
    let workspace = tempfile::tempdir().unwrap();
    let (skill, executed) = FakeSkill::new(
        "fake_computer",
        SkillEffects::mutating(vec![SkillEffect::ComputerUse])
            .with_workspace_scoped(false)
            .with_outside_workspace_capable(true),
        RiskLevel::Moderate,
    );
    let mut registry = SkillRegistry::new();
    registry.add(Box::new(skill));
    let mut harness = AgentHarness::new(registry);

    let developer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let developer_ctx =
        ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(developer);
    let err = harness
        .execute_skill(
            "fake_computer",
            serde_json::json!({"action": "observe"}),
            AgentPhase::Patch,
            &developer_ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::SkillPolicyDenied { .. }));
    assert!(!executed.load(Ordering::SeqCst));

    let computer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Computer),
        Vec::new(),
        "test",
        "unit",
    );
    let computer_ctx =
        ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(computer);
    let result = harness
        .execute_skill(
            "fake_computer",
            serde_json::json!({"action": "observe"}),
            AgentPhase::Patch,
            &computer_ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert_eq!(result.output, "fake skill executed");

    let (skill, executed_by_grant) = FakeSkill::new(
        "fake_computer",
        SkillEffects::mutating(vec![SkillEffect::ComputerUse])
            .with_workspace_scoped(false)
            .with_outside_workspace_capable(true),
        RiskLevel::Moderate,
    );
    let mut registry = SkillRegistry::new();
    registry.add(Box::new(skill));
    let mut harness = AgentHarness::new(registry);
    let granted = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        vec![CapabilityGrant::new(
            CapabilityKind::ComputerUse,
            "observe",
            AccessMode::Execute,
            GrantScope::Permanent,
            "test grant",
        )],
        "test",
        "unit",
    );
    let granted_ctx =
        ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(granted);
    harness
        .execute_skill(
            "fake_computer",
            serde_json::json!({"action": "observe"}),
            AgentPhase::Patch,
            &granted_ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert!(executed_by_grant.load(Ordering::SeqCst));
}

#[test]
fn test_external_send_effect_requires_approval_and_does_not_execute() {
    for profile in [CapabilityProfile::Developer, CapabilityProfile::Full] {
        let workspace = tempfile::tempdir().unwrap();
        let (skill, executed) = FakeSkill::new(
            "fake_external_send",
            SkillEffects::mutating(vec![SkillEffect::ExternalSend])
                .with_workspace_scoped(false)
                .with_outside_workspace_capable(true),
            RiskLevel::Moderate,
        );
        let mut registry = SkillRegistry::new();
        registry.add(Box::new(skill));
        let mut harness = AgentHarness::new(registry);
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(profile),
            Vec::new(),
            "test",
            "unit",
        );
        let mut trust = topagent_core::RunTrustContext::default();
        trust.add_source(SourceLabel::low(
            SourceKind::FetchedWebContent,
            InfluenceMode::MayDriveAction,
            "web_search result",
        ));
        let ctx = ExecutionContext::new(workspace.path().to_path_buf())
            .with_capability_manager(manager)
            .with_approval_mailbox(mailbox)
            .with_run_trust_context(trust);

        let err = harness
            .execute_skill(
                "fake_external_send",
                serde_json::json!({"target": "https://example.com/upload"}),
                AgentPhase::Patch,
                &ctx,
                &RuntimeOptions::default(),
            )
            .unwrap_err();

        assert!(
            matches!(err, Error::ApprovalRequired(ref request) if request.capability.as_ref().is_some_and(|capability| capability.detail.kind == CapabilityKind::ExternalSend)),
            "expected external send approval for {profile:?}, got {err:?}"
        );
        assert!(
            !executed.load(Ordering::SeqCst),
            "external send must not execute before approval under {profile:?}"
        );
    }
}

#[test]
fn test_web_search_effects_phase_exposure_and_workspace_block_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let mut harness = default_harness();
    let developer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(developer);

    let investigate = skill_names(harness.available_skills(&ctx, AgentPhase::Investigate));
    let plan = skill_names(harness.available_skills(&ctx, AgentPhase::Plan));
    let verify = skill_names(harness.available_skills(&ctx, AgentPhase::Verify));
    let finalize = skill_names(harness.available_skills(&ctx, AgentPhase::Finalize));

    assert!(investigate.contains(&"web_search".to_string()));
    assert!(plan.contains(&"web_search".to_string()));
    assert!(!verify.contains(&"web_search".to_string()));
    assert!(!finalize.contains(&"web_search".to_string()));

    let effects = default_effects_for_skill("web_search");
    assert!(effects.read_only);
    assert!(effects.parallel_safe);
    assert!(effects.includes(SkillEffect::WebSearch));
    assert!(effects.includes(SkillEffect::NetworkAccess));

    let workspace_manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let workspace_ctx =
        ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(workspace_manager);
    let workspace_investigate =
        skill_names(harness.available_skills(&workspace_ctx, AgentPhase::Investigate));
    assert!(!workspace_investigate.contains(&"web_search".to_string()));
    let err = harness
        .execute_skill(
            "web_search",
            serde_json::json!({"query": "topagent"}),
            AgentPhase::Investigate,
            &workspace_ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::SkillPolicyDenied { .. }));
}

#[test]
fn test_developer_profile_can_run_web_search_without_approval() {
    let temp = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox.clone());
    let mut harness = web_search_harness();

    let result = harness
        .execute_skill(
            "web_search",
            serde_json::json!({"query": "topagent", "max_results": 3}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert!(result.output.contains("web_search_results"));
    assert!(result.output.contains("low-trust"));
    assert_eq!(mailbox.list().len(), 0);
}

#[test]
fn test_workspace_profile_can_run_web_search_with_explicit_grant() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        vec![CapabilityGrant::new(
            CapabilityKind::WebSearch,
            "web_search",
            AccessMode::Read,
            GrantScope::Permanent,
            "test web search grant",
        )],
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
    let mut harness = web_search_harness();

    let result = harness
        .execute_skill(
            "web_search",
            serde_json::json!({"query": "topagent", "max_results": 3}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert!(result.output.contains("web_search_results"));
    assert!(result.output.contains("TopAgent"));
}

#[test]
fn test_web_search_cannot_run_in_verify_or_finalize_phase() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
    let mut harness = web_search_harness();

    for phase in [AgentPhase::Verify, AgentPhase::Finalize] {
        let err = harness
            .execute_skill(
                "web_search",
                serde_json::json!({"query": "topagent"}),
                phase,
                &ctx,
                &RuntimeOptions::default(),
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::SkillPolicyDenied { .. }),
            "expected phase denial for {phase:?}, got {err:?}"
        );
    }
}

#[test]
fn test_preauthorized_workspace_file_access_does_not_request_approval() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("input.txt"), "workspace content").unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox.clone());
    let mut harness = default_harness();

    let read = harness
        .execute_skill(
            "read",
            serde_json::json!({"path": "input.txt"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert_eq!(read.output, "workspace content");

    harness
        .execute_skill(
            "write",
            serde_json::json!({"path": "output.txt", "content": "written"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join("output.txt")).unwrap(),
        "written"
    );
    assert_eq!(mailbox.list().len(), 0);
}

#[test]
fn test_outside_workspace_access_creates_one_pending_skill_approval_record() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("report.txt");
    fs::write(&outside_path, "outside content").unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let approving_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |request| {
        approving_mailbox
            .approve(&request.id, Some("approved for test".to_string()))
            .unwrap();
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox.clone())
        .with_task_id("task-123")
        .with_session_id("session-abc");
    let mut harness = default_harness();

    let result = harness
        .execute_skill(
            "read",
            serde_json::json!({"path": outside_path.display().to_string()}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert_eq!(result.output, "outside content");
    assert_eq!(mailbox.list().len(), 1);
    let request = mailbox.list().pop().unwrap().request;
    let pending = mailbox
        .pending_skill_execution(&request.id)
        .expect("approval should record the blocked skill call");
    assert_eq!(pending.request_id, request.id);
    assert_eq!(pending.skill_name, "read");
    assert_eq!(pending.phase, "investigate");
    assert_eq!(pending.task_id.as_deref(), Some("task-123"));
    assert_eq!(pending.session_id.as_deref(), Some("session-abc"));
    assert_eq!(
        pending.input,
        serde_json::json!({"path": outside_path.display().to_string()})
    );
}

#[test]
fn test_pending_bash_high_risk_resumes_after_approval_once() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let approving_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |request| {
        approving_mailbox
            .approve(&request.id, Some("approved for test".to_string()))
            .unwrap();
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox.clone())
        .with_task_id("task-bash")
        .with_session_id("session-bash");
    let mut harness = default_harness();

    let result = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "printf approved >> approved.txt"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert!(result.output.contains("Exit code: 0"));
    assert_eq!(
        fs::read_to_string(workspace.path().join("approved.txt")).unwrap(),
        "approved"
    );
    let request = mailbox.list().pop().unwrap().request;
    let pending = mailbox
        .pending_skill_execution(&request.id)
        .expect("approval should record the blocked bash skill call");
    assert_eq!(pending.skill_name, "bash");
    assert_eq!(pending.phase, "patch");
    assert_eq!(pending.task_id.as_deref(), Some("task-bash"));
    assert_eq!(pending.session_id.as_deref(), Some("session-bash"));
}

#[test]
fn test_denied_pending_bash_approval_does_not_execute() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let denying_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |request| {
        denying_mailbox
            .deny(&request.id, Some("denied for test".to_string()))
            .unwrap();
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox);
    let mut harness = default_harness();

    let err = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "printf denied >> denied.txt"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::Denied { .. })),
        "expected denied capability error, got {err:?}"
    );
    assert!(!workspace.path().join("denied.txt").exists());
}

#[test]
fn test_superseded_pending_bash_approval_does_not_execute() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let superseding_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |_request| {
        superseding_mailbox.supersede_pending("superseded in test");
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox);
    let mut harness = default_harness();

    let err = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "printf superseded >> superseded.txt"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected superseded approval to stop execution, got {err:?}"
    );
    assert!(!workspace.path().join("superseded.txt").exists());
}

#[test]
fn test_expired_pending_bash_approval_does_not_execute() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let expiring_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |_request| {
        expiring_mailbox.expire_pending("expired in test");
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox);
    let mut harness = default_harness();

    let err = harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "printf expired >> expired.txt"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected expired approval to stop execution, got {err:?}"
    );
    assert!(!workspace.path().join("expired.txt").exists());
}

#[test]
fn test_once_approval_is_consumed_after_one_execution() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let approving_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |request| {
        approving_mailbox
            .approve_with_scope(
                &request.id,
                GrantScope::Once,
                Some("approved once for test".to_string()),
            )
            .unwrap();
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager.clone())
        .with_approval_mailbox(mailbox);
    let mut harness = default_harness();

    harness
        .execute_skill(
            "bash",
            serde_json::json!({"command": "printf once >> once.txt"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert_eq!(
        fs::read_to_string(workspace.path().join("once.txt")).unwrap(),
        "once"
    );
    assert!(
        manager.grants().is_empty(),
        "once grant should be consumed and removed after the approved execution"
    );
}

#[test]
fn test_task_scoped_approval_cannot_be_reused_by_another_task() {
    let workspace = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
    let approving_mailbox = mailbox.clone();
    mailbox.set_notifier(Arc::new(move |request| {
        approving_mailbox
            .approve_with_scope(
                &request.id,
                GrantScope::ThisTask,
                Some("approved for task-a".to_string()),
            )
            .unwrap();
    }));
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let command = serde_json::json!({"command": "printf task > scoped.txt"});
    let ctx_task_a = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager.clone())
        .with_approval_mailbox(mailbox)
        .with_task_id("task-a");
    let mut harness = default_harness();

    harness
        .execute_skill(
            "bash",
            command.clone(),
            AgentPhase::Patch,
            &ctx_task_a,
            &RuntimeOptions::default(),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path().join("scoped.txt")).unwrap(),
        "task"
    );

    fs::remove_file(workspace.path().join("scoped.txt")).unwrap();
    let ctx_task_b = ExecutionContext::new(workspace.path().to_path_buf())
        .with_capability_manager(manager)
        .with_task_id("task-b");
    let err = harness
        .execute_skill(
            "bash",
            command,
            AgentPhase::Patch,
            &ctx_task_b,
            &RuntimeOptions::default(),
        )
        .unwrap_err();

    assert!(
        matches!(err, Error::Capability(ref error) if matches!(**error, topagent_core::CapabilityError::NeedsApproval { .. })),
        "expected task-scoped grant not to match task-b, got {err:?}"
    );
    assert!(!workspace.path().join("scoped.txt").exists());
}

#[cfg(feature = "computer-use")]
#[test]
fn test_preauthorized_computer_use_profile_does_not_request_duplicate_approval() {
    let temp = tempfile::tempdir().unwrap();
    let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Computer),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf())
        .with_capability_manager(manager)
        .with_approval_mailbox(mailbox.clone());
    let mut harness = default_harness();

    let result = harness
        .execute_skill(
            "computer_use",
            serde_json::json!({"action": "observe"}),
            AgentPhase::Patch,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap();

    assert!(result
        .output
        .contains("computer_use scaffold-only response"));
    assert!(result.output.contains("was not performed"));
    assert_eq!(mailbox.list().len(), 0);
}

#[test]
fn test_full_profile_still_requires_approval_for_secret_reads_through_harness() {
    let temp = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Full),
        Vec::new(),
        "test",
        "unit",
    );
    let ctx = ExecutionContext::new(temp.path().to_path_buf()).with_capability_manager(manager);
    let mut harness = default_harness();

    let err = harness
        .execute_skill(
            "read",
            serde_json::json!({"path": "/home/operator/.ssh/id_ed25519"}),
            AgentPhase::Investigate,
            &ctx,
            &RuntimeOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Capability(ref error)
                if matches!(
                    **error,
                    topagent_core::CapabilityError::NeedsApproval {
                        kind: CapabilityKind::SecretRead,
                        ..
                    }
                )
        ),
        "expected secret read approval, got {err:?}"
    );
}

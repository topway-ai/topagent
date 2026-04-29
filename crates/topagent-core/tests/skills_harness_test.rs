use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use topagent_core::harness::{AgentHarness, AgentPhase};
use topagent_core::skills::{
    default_effects_for_skill, Skill, SkillContext, SkillEffect, SkillEffects, SkillInput,
    SkillRegistry, SkillResult,
};
use topagent_core::tools::{default_tools, SaveNoteTool};
use topagent_core::{
    AccessConfig, AccessMode, CapabilityGrant, CapabilityKind, CapabilityManager,
    CapabilityProfile, Error, ExecutionContext, GrantScope, RiskLevel, RuntimeOptions, SkillSchema,
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

fn skill_names(specs: Vec<topagent_core::ToolSpec>) -> Vec<String> {
    specs.into_iter().map(|spec| spec.name).collect()
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
fn test_web_search_scaffold_effects_and_phase_exposure_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let mut registry = SkillRegistry::new();
    for tool in default_tools().into_inner() {
        registry.add_tool(tool);
    }
    let mut harness = AgentHarness::new(registry);
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

    assert!(investigate.contains(&"web_search".to_string()));
    assert!(plan.contains(&"web_search".to_string()));
    assert!(!verify.contains(&"web_search".to_string()));

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

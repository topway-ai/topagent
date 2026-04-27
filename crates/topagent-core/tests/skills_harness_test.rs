use topagent_core::harness::{AgentHarness, AgentPhase};
use topagent_core::skills::{default_effects_for_skill, SkillEffect, SkillRegistry};
use topagent_core::tools::{default_tools, SaveNoteTool};
use topagent_core::{AccessConfig, CapabilityManager, CapabilityProfile, ExecutionContext};

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

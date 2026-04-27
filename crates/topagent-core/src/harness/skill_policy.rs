use crate::capability::{CapabilityKind, CapabilityProfile};
use crate::context::ExecutionContext;
use crate::skills::{Skill, SkillEffect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentPhase {
    Investigate,
    Plan,
    Patch,
    Verify,
    Finalize,
}

impl AgentPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Investigate => "investigate",
            Self::Plan => "plan",
            Self::Patch => "patch",
            Self::Verify => "verify",
            Self::Finalize => "finalize",
        }
    }
}

pub fn skill_allowed_in_phase(skill: &dyn Skill, phase: AgentPhase) -> bool {
    let name = skill.name();
    let effects = skill.effects();

    match phase {
        AgentPhase::Investigate => effects.read_only || matches!(name, "bash" | "update_plan"),
        AgentPhase::Plan => effects.read_only || matches!(name, "bash" | "update_plan"),
        AgentPhase::Patch => {
            !matches!(name, "save_note" | "manage_operator_preference")
                || effects.includes(SkillEffect::MemoryRead)
        }
        AgentPhase::Verify => effects.read_only || matches!(name, "bash" | "update_plan"),
        AgentPhase::Finalize => {
            effects.read_only || matches!(name, "save_note" | "manage_operator_preference")
        }
    }
}

pub fn skill_allowed_by_access(skill: &dyn Skill, ctx: &ExecutionContext) -> bool {
    let effects = skill.effects();
    if !effects.includes(SkillEffect::ComputerUse) {
        return true;
    }

    let Some(manager) = ctx.capability_manager() else {
        return false;
    };

    let config = manager.config();
    matches!(
        config.profile,
        CapabilityProfile::Computer | CapabilityProfile::Full
    ) || manager
        .grants()
        .iter()
        .any(|grant| grant.kind == CapabilityKind::ComputerUse && !grant.is_expired())
}

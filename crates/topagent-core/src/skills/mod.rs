mod effects;
mod registry;
mod result;
mod schema;

pub use effects::{SkillEffect, SkillEffects};
pub use registry::{default_effects_for_skill, SkillRegistry, ToolBackedSkill};
pub use result::{SkillContext, SkillInput, SkillOutput, SkillResult};
pub use schema::SkillSchema;

use crate::capability::RiskLevel;

pub trait Skill: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> SkillSchema;
    fn effects(&self) -> SkillEffects;
    fn risk(&self, input: &SkillInput, ctx: &SkillContext<'_>) -> RiskLevel;
    fn execute(&self, input: SkillInput, ctx: &SkillContext<'_>) -> SkillResult;
}

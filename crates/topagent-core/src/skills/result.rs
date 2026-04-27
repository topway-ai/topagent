use crate::context::ToolContext;

pub type SkillContext<'a> = ToolContext<'a>;
pub type SkillInput = serde_json::Value;
pub type SkillOutput = String;
pub type SkillResult = crate::Result<SkillOutput>;

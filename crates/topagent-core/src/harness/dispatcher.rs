use crate::context::ToolContext;
use crate::skills::{SkillEffects, SkillInput, SkillRegistry};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct SkillExecution {
    pub name: String,
    pub output: String,
    pub effects: SkillEffects,
    pub risk: crate::capability::RiskLevel,
}

pub struct SkillDispatcher {
    registry: SkillRegistry,
    execution_count: usize,
}

impl SkillDispatcher {
    pub fn new(registry: SkillRegistry) -> Self {
        Self {
            registry,
            execution_count: 0,
        }
    }

    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }

    pub fn has_skill(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    pub fn execution_count(&self) -> usize {
        self.execution_count
    }

    pub fn execute(
        &mut self,
        name: &str,
        input: SkillInput,
        ctx: &ToolContext<'_>,
    ) -> Result<SkillExecution> {
        let Some(skill) = self.registry.get(name) else {
            return Err(Error::ToolFailed(format!("unknown skill '{}'", name)));
        };
        let effects = skill.effects();
        let risk = skill.risk(&input, ctx);
        let output = skill.execute(input, ctx)?;
        self.execution_count += 1;
        Ok(SkillExecution {
            name: name.to_string(),
            output,
            effects,
            risk,
        })
    }
}

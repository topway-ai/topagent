use crate::context::{ExecutionContext, ToolContext};
use crate::harness::context::ContextBundle;
use crate::harness::dispatcher::{SkillDispatcher, SkillExecution};
use crate::harness::skill_policy::{skill_allowed_by_access, skill_allowed_in_phase, AgentPhase};
use crate::runtime::RuntimeOptions;
use crate::skills::{SkillInput, SkillRegistry};
use crate::{Result, ToolSpec};

pub struct AgentHarness {
    dispatcher: SkillDispatcher,
}

impl AgentHarness {
    pub fn new(skills: SkillRegistry) -> Self {
        Self {
            dispatcher: SkillDispatcher::new(skills),
        }
    }

    pub fn build_context_for_task(&self, ctx: &ExecutionContext) -> ContextBundle {
        ContextBundle::from_execution_context(ctx)
    }

    pub fn available_skills(&self, ctx: &ExecutionContext, phase: AgentPhase) -> Vec<ToolSpec> {
        self.dispatcher
            .registry()
            .iter()
            .filter(|skill| skill_allowed_in_phase(*skill, phase))
            .filter(|skill| skill_allowed_by_access(*skill, ctx))
            .map(|skill| skill.schema().as_tool_spec())
            .collect()
    }

    pub fn skill_specs(&self) -> Vec<ToolSpec> {
        self.dispatcher.registry().specs()
    }

    pub fn has_skill(&self, name: &str) -> bool {
        self.dispatcher.has_skill(name)
    }

    pub fn execute_skill(
        &mut self,
        name: &str,
        input: SkillInput,
        ctx: &ExecutionContext,
        runtime: &RuntimeOptions,
    ) -> Result<SkillExecution> {
        let skill_ctx = ToolContext::new(ctx, runtime);
        self.dispatcher.execute(name, input, &skill_ctx)
    }

    pub fn dispatch_count(&self) -> usize {
        self.dispatcher.execution_count()
    }
}

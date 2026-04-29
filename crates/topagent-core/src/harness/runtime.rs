use crate::approval::PendingSkillExecutionDraft;
use crate::context::{ExecutionContext, ToolContext};
use crate::harness::context::ContextBundle;
use crate::harness::dispatcher::{SkillDispatcher, SkillExecution};
use crate::harness::skill_policy::{
    capability_requests_for_skill, skill_allowed_by_access, skill_allowed_for_execution,
    skill_allowed_in_phase, AgentPhase,
};
use crate::runtime::RuntimeOptions;
use crate::skills::{SkillInput, SkillRegistry};
use crate::{Error, Result, ToolSpec};

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
        phase: AgentPhase,
        ctx: &ExecutionContext,
        runtime: &RuntimeOptions,
    ) -> Result<SkillExecution> {
        let Some(skill) = self.dispatcher.registry().get(name) else {
            return Err(Error::ToolNotFound(name.to_string()));
        };

        if let Err(reason) = skill_allowed_for_execution(skill, phase, &input) {
            return Err(Error::SkillPolicyDenied {
                skill: name.to_string(),
                phase: phase.as_str().to_string(),
                reason,
            });
        }

        if !skill_allowed_by_access(skill, ctx) {
            return Err(Error::SkillPolicyDenied {
                skill: name.to_string(),
                phase: phase.as_str().to_string(),
                reason: "skill is not allowed by the current access profile or grants".to_string(),
            });
        }

        let effects = skill.effects();
        let base_ctx = ToolContext::new(ctx, runtime);
        let risk = skill.risk(&input, &base_ctx);
        let capability_requests = capability_requests_for_skill(name, &effects, &input, risk, ctx)?;
        for request in &capability_requests {
            ctx.authorize_capability_for_pending_skill(
                request.clone(),
                PendingSkillExecutionDraft {
                    skill_name: name.to_string(),
                    input: input.clone(),
                    phase: phase.as_str().to_string(),
                    task_id: ctx.task_id().map(ToOwned::to_owned),
                    session_id: ctx.session_id().map(ToOwned::to_owned),
                },
            )?;
        }

        let skill_ctx = ToolContext::new(ctx, runtime).with_preauthorized(capability_requests);
        self.dispatcher.execute(name, input, &skill_ctx)
    }

    pub fn dispatch_count(&self) -> usize {
        self.dispatcher.execution_count()
    }
}

use super::{Agent, ExecutionStage};
use crate::behavior::{BashCommandClass, BehaviorContract};
use crate::context::ExecutionContext;
use crate::harness::AgentPhase;

impl Agent {
    pub fn classify_bash_command(cmd: &str) -> BashCommandClass {
        BehaviorContract::default().classify_bash_command(cmd)
    }

    pub(super) fn current_agent_phase(&self) -> AgentPhase {
        if self.planning.is_active() && !self.plan_exists() {
            return AgentPhase::Plan;
        }
        if self.planning.is_active() && self.plan_exists() {
            return AgentPhase::Patch;
        }

        match self.execution_stage {
            ExecutionStage::Research => AgentPhase::Investigate,
            ExecutionStage::Edit => AgentPhase::Patch,
            ExecutionStage::Review => AgentPhase::Verify,
        }
    }

    pub(super) fn agent_phase_for_tool_execution(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> AgentPhase {
        if self.planning.is_active() && !self.plan_exists() {
            return AgentPhase::Plan;
        }

        if self.behavior.is_memory_write_tool(name) {
            return AgentPhase::Finalize;
        }

        if name == "bash" {
            let command = args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            return match self.behavior.classify_bash_command(command) {
                BashCommandClass::MutationRisk => AgentPhase::Patch,
                BashCommandClass::Verification => AgentPhase::Verify,
                BashCommandClass::ResearchSafe => AgentPhase::Investigate,
            };
        }

        if self.behavior.is_mutation_tool(name) {
            return AgentPhase::Patch;
        }

        self.current_agent_phase()
    }

    pub(super) fn sync_provider_tools(&mut self, ctx: &ExecutionContext) {
        let phase = self.current_agent_phase();
        self.provider
            .set_tool_specs(self.harness.available_skills(ctx, phase));
    }
}

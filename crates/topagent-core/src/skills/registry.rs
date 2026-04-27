use crate::capability::{assess_computer_action, assess_shell_command, RiskLevel};
use crate::skills::{Skill, SkillContext, SkillEffect, SkillEffects, SkillInput, SkillResult};
use crate::tools::Tool;
use crate::ToolSpec;
use std::collections::HashMap;

pub struct SkillRegistry {
    skills: Vec<Box<dyn Skill>>,
    by_name: HashMap<String, usize>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn add(&mut self, skill: Box<dyn Skill>) {
        let name = skill.name().to_string();
        if !self.by_name.contains_key(&name) {
            let idx = self.skills.len();
            self.by_name.insert(name, idx);
            self.skills.push(skill);
        }
    }

    pub fn add_tool(&mut self, tool: Box<dyn Tool>) {
        self.add(Box::new(ToolBackedSkill::from_tool(tool)));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Skill> {
        self.by_name
            .get(name)
            .and_then(|&idx| self.skills.get(idx).map(|skill| skill.as_ref()))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Skill> {
        self.skills.iter().map(|skill| skill.as_ref())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.skills
            .iter()
            .map(|skill| skill.schema().as_tool_spec())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ToolBackedSkill {
    tool: Box<dyn Tool>,
    spec: ToolSpec,
    effects: SkillEffects,
}

impl ToolBackedSkill {
    pub fn new(tool: Box<dyn Tool>, effects: SkillEffects) -> Self {
        let spec = tool.spec();
        Self {
            tool,
            spec,
            effects,
        }
    }

    pub fn from_tool(tool: Box<dyn Tool>) -> Self {
        let spec = tool.spec();
        let effects = default_effects_for_skill(&spec.name);
        Self {
            tool,
            spec,
            effects,
        }
    }
}

impl Skill for ToolBackedSkill {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn schema(&self) -> crate::skills::SkillSchema {
        self.spec.clone().into()
    }

    fn effects(&self) -> SkillEffects {
        self.effects.clone()
    }

    fn risk(&self, input: &SkillInput, _ctx: &SkillContext<'_>) -> RiskLevel {
        risk_for_skill(self.name(), input)
    }

    fn execute(&self, input: SkillInput, ctx: &SkillContext<'_>) -> SkillResult {
        self.tool.execute(input, ctx)
    }
}

pub fn default_effects_for_skill(name: &str) -> SkillEffects {
    match name {
        "read" => SkillEffects::read_only(vec![SkillEffect::ReadFilesystem])
            .with_outside_workspace_capable(true),
        "write" => SkillEffects::mutating(vec![SkillEffect::WriteFilesystem])
            .with_outside_workspace_capable(true),
        "edit" => SkillEffects::mutating(vec![
            SkillEffect::ReadFilesystem,
            SkillEffect::WriteFilesystem,
        ])
        .with_outside_workspace_capable(true),
        "bash" => SkillEffects::mutating(vec![
            SkillEffect::ExecuteCommand,
            SkillEffect::NetworkAccess,
            SkillEffect::GitRead,
            SkillEffect::GitWrite,
            SkillEffect::PackageInstall,
            SkillEffect::ExternalSend,
            SkillEffect::SystemChange,
        ])
        .with_destructive(true)
        .with_workspace_scoped(false)
        .with_outside_workspace_capable(true),
        "git_status" | "git_diff" | "git_branch" => {
            SkillEffects::read_only(vec![SkillEffect::GitRead])
        }
        "git_clone" => SkillEffects::mutating(vec![
            SkillEffect::GitWrite,
            SkillEffect::NetworkAccess,
            SkillEffect::WriteFilesystem,
        ]),
        "git_add" | "git_commit" => SkillEffects::mutating(vec![SkillEffect::GitWrite]),
        "save_note" => SkillEffects::mutating(vec![SkillEffect::MemoryWrite]),
        "manage_operator_preference" => {
            SkillEffects::mutating(vec![SkillEffect::MemoryWrite]).with_destructive(true)
        }
        "update_plan" => SkillEffects::mutating(Vec::new()).with_parallel_safe(false),
        "computer_use" => SkillEffects::mutating(vec![SkillEffect::ComputerUse])
            .with_workspace_scoped(false)
            .with_outside_workspace_capable(true),
        _ => SkillEffects::mutating(Vec::new()).with_workspace_scoped(false),
    }
}

fn risk_for_skill(name: &str, input: &SkillInput) -> RiskLevel {
    match name {
        "read" | "git_status" | "git_diff" | "git_branch" => RiskLevel::Safe,
        "write" | "edit" | "git_clone" | "git_add" | "save_note" => RiskLevel::Moderate,
        "git_commit" => RiskLevel::High,
        "manage_operator_preference" => {
            match input.get("action").and_then(|value| value.as_str()) {
                Some("list") => RiskLevel::Safe,
                _ => RiskLevel::High,
            }
        }
        "bash" => input
            .get("command")
            .and_then(|value| value.as_str())
            .map(|command| assess_shell_command(command).risk)
            .unwrap_or(RiskLevel::High),
        "computer_use" => {
            let action = input
                .get("action")
                .and_then(|value| value.as_str())
                .unwrap_or("observe");
            let target = input
                .get("target")
                .or_else(|| input.get("text"))
                .and_then(|value| value.as_str())
                .unwrap_or(action);
            assess_computer_action(action, target).0
        }
        _ => RiskLevel::Moderate,
    }
}

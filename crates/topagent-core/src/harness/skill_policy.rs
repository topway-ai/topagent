use crate::behavior::{BashCommandClass, BehaviorContract};
use crate::capability::{
    assess_computer_action, assess_shell_command, AccessMode, CapabilityKind, CapabilityProfile,
    CapabilityRequest, RiskLevel,
};
use crate::context::ExecutionContext;
use crate::skills::{Skill, SkillEffect, SkillEffects, SkillInput};
use crate::Result;

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

    if name == "web_search" {
        return matches!(phase, AgentPhase::Investigate | AgentPhase::Plan);
    }

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

pub fn skill_allowed_for_execution(
    skill: &dyn Skill,
    phase: AgentPhase,
    input: &SkillInput,
) -> std::result::Result<(), String> {
    if skill.name() == "bash" {
        return bash_allowed_for_execution(phase, input);
    }

    if skill_allowed_in_phase(skill, phase) {
        Ok(())
    } else {
        Err("skill is not allowed in the current agent phase".to_string())
    }
}

fn bash_allowed_for_execution(
    phase: AgentPhase,
    input: &SkillInput,
) -> std::result::Result<(), String> {
    let command = string_field(input, "command").unwrap_or_default();
    let class = BehaviorContract::default().classify_bash_command(&command);
    let allowed = match class {
        BashCommandClass::ResearchSafe => {
            matches!(phase, AgentPhase::Investigate | AgentPhase::Plan)
        }
        BashCommandClass::Verification => phase == AgentPhase::Verify,
        BashCommandClass::MutationRisk => phase == AgentPhase::Patch,
    };

    if allowed {
        return Ok(());
    }

    let allowed_phase = match class {
        BashCommandClass::ResearchSafe => "investigate or plan",
        BashCommandClass::Verification => "verify",
        BashCommandClass::MutationRisk => "patch",
    };
    Err(format!(
        "bash command classified as {} is allowed only in {allowed_phase} phase",
        bash_class_label(class)
    ))
}

fn bash_class_label(class: BashCommandClass) -> &'static str {
    match class {
        BashCommandClass::ResearchSafe => "research_safe",
        BashCommandClass::Verification => "verification",
        BashCommandClass::MutationRisk => "mutation_risk",
    }
}

pub fn skill_allowed_by_access(skill: &dyn Skill, ctx: &ExecutionContext) -> bool {
    let effects = skill.effects();

    if effects.includes(SkillEffect::ComputerUse) {
        return feature_allowed_by_access(ctx, CapabilityKind::ComputerUse, |profile| {
            matches!(
                profile,
                CapabilityProfile::Computer | CapabilityProfile::Full
            )
        });
    }

    if effects.includes(SkillEffect::WebSearch) {
        return feature_allowed_by_access(ctx, CapabilityKind::WebSearch, |profile| {
            matches!(
                profile,
                CapabilityProfile::Developer
                    | CapabilityProfile::Computer
                    | CapabilityProfile::Full
            )
        });
    }

    true
}

pub(crate) fn capability_requests_for_skill(
    name: &str,
    effects: &SkillEffects,
    input: &SkillInput,
    risk: RiskLevel,
    ctx: &ExecutionContext,
) -> Result<Vec<CapabilityRequest>> {
    let mut requests = Vec::new();

    if name == "bash" && effects.includes(SkillEffect::ExecuteCommand) {
        let command = string_field(input, "command").unwrap_or_default();
        let assessment = assess_shell_command(&command);
        if assessment.network_required {
            requests.push(CapabilityRequest::new(
                CapabilityKind::Network,
                command.clone(),
                AccessMode::Read,
                RiskLevel::Safe,
                "shell command requires network access",
            ));
        }
        requests.push(CapabilityRequest::new(
            assessment.kind,
            command,
            AccessMode::Execute,
            assessment.risk,
            assessment.reason,
        ));
        return Ok(dedupe_requests(requests));
    }

    if effects.includes(SkillEffect::ReadFilesystem)
        || effects.includes(SkillEffect::WriteFilesystem)
        || effects.includes(SkillEffect::SecretAccess)
    {
        let mode = filesystem_mode(effects);
        if let Some(path) = string_field(input, "path") {
            let (_, request) = ctx.capability_request_for_path(
                &path,
                mode,
                format!("skill `{name}` declares filesystem access effects"),
            )?;
            requests.push(request);
        } else if effects.includes(SkillEffect::WriteFilesystem) {
            requests.push(CapabilityRequest::new(
                CapabilityKind::Filesystem,
                fallback_target(input, name),
                mode,
                risk,
                format!("skill `{name}` declares filesystem write effects"),
            ));
        }
    }

    if effects.includes(SkillEffect::WebSearch) {
        requests.push(CapabilityRequest::new(
            CapabilityKind::WebSearch,
            "web_search",
            AccessMode::Read,
            RiskLevel::Safe,
            web_search_reason(input),
        ));
    }

    if effects.includes(SkillEffect::NetworkAccess) && !matches!(name, "bash" | "web_search") {
        let target = fallback_target(input, name);
        requests.push(CapabilityRequest::new(
            CapabilityKind::Network,
            target,
            AccessMode::Read,
            network_risk(risk),
            format!("skill `{name}` declares network access effects"),
        ));
    }

    if effects.includes(SkillEffect::ComputerUse) {
        let action = string_field(input, "action").unwrap_or_else(|| "observe".to_string());
        let target = string_field(input, "target")
            .or_else(|| string_field(input, "text"))
            .unwrap_or_else(|| action.clone());
        let (risk, reason) = assess_computer_action(&action, &target);
        requests.push(CapabilityRequest::new(
            CapabilityKind::ComputerUse,
            target,
            AccessMode::Execute,
            risk,
            reason,
        ));
    }

    if effects.includes(SkillEffect::GitRead) && name != "bash" {
        requests.push(CapabilityRequest::new(
            CapabilityKind::Git,
            git_target(name, input),
            AccessMode::Execute,
            RiskLevel::Safe,
            format!("skill `{name}` declares git read effects"),
        ));
    }

    if effects.includes(SkillEffect::GitWrite) && name != "bash" {
        requests.push(CapabilityRequest::new(
            CapabilityKind::Git,
            git_target(name, input),
            AccessMode::Execute,
            git_write_risk(name, risk),
            format!("skill `{name}` declares git write effects"),
        ));
    }

    if effects.includes(SkillEffect::PackageInstall) && name != "bash" {
        requests.push(CapabilityRequest::new(
            CapabilityKind::PackageManager,
            fallback_target(input, name),
            AccessMode::Execute,
            risk,
            format!("skill `{name}` declares package manager effects"),
        ));
    }

    if effects.includes(SkillEffect::ExternalSend) && name != "bash" {
        requests.push(CapabilityRequest::new(
            CapabilityKind::ExternalSend,
            fallback_target(input, name),
            AccessMode::Execute,
            risk,
            format!("skill `{name}` declares external send effects"),
        ));
    }

    if effects.includes(SkillEffect::SystemChange) && name != "bash" {
        requests.push(CapabilityRequest::new(
            CapabilityKind::SystemService,
            fallback_target(input, name),
            AccessMode::Execute,
            risk,
            format!("skill `{name}` declares system change effects"),
        ));
    }

    if effects.includes(SkillEffect::MemoryWrite) && risk != RiskLevel::Safe {
        requests.push(CapabilityRequest::new(
            CapabilityKind::MemoryWrite,
            memory_target(name, input),
            AccessMode::Write,
            risk,
            format!("skill `{name}` declares durable memory write effects"),
        ));
    }

    Ok(dedupe_requests(requests))
}

fn feature_allowed_by_access(
    ctx: &ExecutionContext,
    kind: CapabilityKind,
    profile_allows: impl FnOnce(CapabilityProfile) -> bool,
) -> bool {
    let Some(manager) = ctx.capability_manager() else {
        return false;
    };

    let config = manager.config();
    profile_allows(config.profile)
        || manager
            .grants()
            .iter()
            .any(|grant| grant.kind == kind && !grant.is_expired())
}

fn filesystem_mode(effects: &SkillEffects) -> AccessMode {
    match (
        effects.includes(SkillEffect::ReadFilesystem),
        effects.includes(SkillEffect::WriteFilesystem),
    ) {
        (true, true) => AccessMode::ReadWrite,
        (false, true) => AccessMode::Write,
        _ => AccessMode::Read,
    }
}

fn string_field(input: &SkillInput, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn fallback_target(input: &SkillInput, name: &str) -> String {
    for key in ["target", "url", "query", "command", "path"] {
        if let Some(value) = string_field(input, key) {
            return value;
        }
    }
    name.to_string()
}

fn network_risk(risk: RiskLevel) -> RiskLevel {
    if risk.is_high_impact() {
        risk
    } else {
        RiskLevel::Safe
    }
}

fn git_write_risk(name: &str, risk: RiskLevel) -> RiskLevel {
    match name {
        "git_commit" => RiskLevel::Moderate,
        _ => risk,
    }
}

fn git_target(name: &str, input: &SkillInput) -> String {
    match name {
        "git_status" => "git status".to_string(),
        "git_diff" => "git diff".to_string(),
        "git_branch" => "git branch".to_string(),
        "git_add" => "git add".to_string(),
        "git_commit" => "git commit".to_string(),
        "git_clone" => string_field(input, "url").unwrap_or_else(|| "git clone".to_string()),
        _ => fallback_target(input, name),
    }
}

fn memory_target(name: &str, input: &SkillInput) -> String {
    match name {
        "save_note" => ".topagent/notes".to_string(),
        "manage_operator_preference" => "USER.md".to_string(),
        _ => fallback_target(input, name),
    }
}

fn web_search_reason(input: &SkillInput) -> String {
    let query = string_field(input, "query").unwrap_or_else(|| "<missing query>".to_string());
    format!(
        "web_search query `{}`; remote content is low trust and must not be executed",
        compact(&query, 120)
    )
}

fn compact(value: &str, max_len: usize) -> String {
    let compacted = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.len() <= max_len {
        compacted
    } else {
        format!("{}...", &compacted[..max_len.saturating_sub(3)])
    }
}

fn dedupe_requests(requests: Vec<CapabilityRequest>) -> Vec<CapabilityRequest> {
    let mut deduped = Vec::new();
    for request in requests {
        if deduped.iter().any(|existing: &CapabilityRequest| {
            existing.kind == request.kind
                && existing.target == request.target
                && existing.mode == request.mode
        }) {
            continue;
        }
        deduped.push(request);
    }
    deduped
}

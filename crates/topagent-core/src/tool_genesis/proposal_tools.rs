use super::{
    chrono_timestamp, get_current_timestamp, get_optional_string, get_string, ProposalStatus,
    ToolDesign, ToolGenesis, ToolInput, VerificationPlan, VerificationSpec,
};
use crate::context::ToolContext;
use crate::error::Error;
use crate::tool_spec::ToolSpec;
use crate::tools::Tool;
use crate::Result;

pub struct DesignToolTool;

impl Default for DesignToolTool {
    fn default() -> Self {
        Self::new()
    }
}

impl DesignToolTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for DesignToolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "design_tool".to_string(),
            description: "design a new tool from a requirement; creates a proposal that can be implemented, revised, or discarded".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "requirement": {
                        "type": "string",
                        "description": "abstract natural language requirement for the tool, e.g. 'I need a tool that searches for files by name pattern'"
                    },
                    "name": {
                        "type": "string",
                        "description": "proposed unique name for the tool (alphanumeric + underscore only)"
                    },
                    "description": {
                        "type": "string",
                        "description": "human-readable description of what the tool will do"
                    },
                    "rationale": {
                        "type": "string",
                        "description": "brief explanation of why this tool is needed and how it fits the workflow"
                    },
                    "inputs": {
                        "type": "array",
                        "description": "named inputs the tool will accept",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "description": { "type": "string" },
                                "required": { "type": "boolean" }
                            },
                            "required": ["name", "description"]
                        }
                    },
                    "argv_template": {
                        "type": "array",
                        "description": "command argv template with {var_name} placeholders, e.g. ['./script.sh', '{path}', '--flag', '{pattern}']"
                    },
                    "verification_args": {
                        "type": "array",
                        "description": "arguments to pass during verification",
                        "items": { "type": "string" }
                    },
                    "expected_exit": {
                        "type": "integer",
                        "description": "expected exit code for verification (default: 0)"
                    },
                    "expected_output_contains": {
                        "type": "string",
                        "description": "optional string that must appear in verification output"
                    }
                },
                "required": ["requirement", "name", "description", "rationale", "verification_args"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let requirement = get_string(&args, "requirement")?;
        let name = get_string(&args, "name")?;
        let description = get_string(&args, "description")?;
        let rationale = get_string(&args, "rationale")?;

        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(Error::InvalidInput(
                "tool name must be alphanumeric + underscore only".to_string(),
            ));
        }

        let design = ToolDesign {
            requirement,
            name: name.clone(),
            description,
            rationale,
            inputs: parse_inputs(&args),
            argv_template: parse_string_array(&args, "argv_template"),
            verification: VerificationPlan {
                verification_args: parse_string_array(&args, "verification_args"),
                expected_exit: args
                    .get("expected_exit")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0) as i32,
                expected_output_contains: args
                    .get("expected_output_contains")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(String::from),
            },
            status: ProposalStatus::Proposed,
            created_at: chrono_timestamp(),
            ..ToolDesign::default()
        };

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal_path = genesis.save_proposal(&design)?;
        Ok(format!(
            "tool design proposal saved for review\n\
             proposal: {}\n\
             run implement_tool_proposal to create the tool from this design",
            proposal_path.display()
        ))
    }
}

pub struct ImplementToolProposalTool;

impl Default for ImplementToolProposalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ImplementToolProposalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ImplementToolProposalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "implement_tool_proposal".to_string(),
            description:
                "implement a tool from an approved design proposal; creates and verifies the tool"
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool proposal to implement (without .json extension)"
                    },
                    "script": {
                        "type": "string",
                        "description": "shell script content that implements the tool"
                    }
                },
                "required": ["name", "script"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let script = get_string(&args, "script")?;
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal = genesis.load_proposal(&name)?;

        match proposal.status {
            ProposalStatus::Approved => {}
            ProposalStatus::Proposed => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' is not approved (status: {})",
                    name, proposal.status
                )));
            }
            ProposalStatus::Implemented => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' has already been implemented",
                    name
                )));
            }
            ProposalStatus::Verified => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' has already been verified",
                    name
                )));
            }
            ProposalStatus::Rejected => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' was rejected",
                    name
                )));
            }
        }

        let verification = VerificationSpec {
            verification_args: proposal.verification.verification_args,
            expected_exit: proposal.verification.expected_exit,
            expected_output_contains: proposal.verification.expected_output_contains,
        };

        let result = genesis.create_tool(
            &proposal.requirement,
            &proposal.name,
            &proposal.description,
            &script,
            proposal.inputs,
            proposal.argv_template,
            Some(verification),
        )?;

        if result.success {
            genesis.update_proposal_status(&name, ProposalStatus::Verified)?;
            let script_path = genesis.tools_dir().join(&name).join("script.sh");
            Ok(format!(
                "tool '{}' created and verified successfully from proposal\npath: {}",
                result.tool_name,
                script_path.display()
            ))
        } else {
            genesis.update_proposal_status(&name, ProposalStatus::Implemented)?;
            Ok(format!(
                "tool '{}' created but verification failed: {}\n\
                 use repair_tool to fix and re-verify",
                result.tool_name, result.message
            ))
        }
    }
}

pub struct ListToolProposalsTool;

impl Default for ListToolProposalsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ListToolProposalsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ListToolProposalsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_tool_proposals".to_string(),
            description: "list all tool design proposals in .topagent/tool-genesis/proposals/"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn execute(&self, _args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposals = genesis.list_proposals()?;
        if proposals.is_empty() {
            return Ok("no tool proposals found in .topagent/tool-genesis/proposals/".to_string());
        }

        let lines: Vec<_> = proposals
            .into_iter()
            .map(|proposal| {
                format!(
                    "- [{}] {}: {}",
                    proposal.status, proposal.name, proposal.description
                )
            })
            .collect();
        Ok(lines.join("\n"))
    }
}

pub struct ShowToolProposalTool;

impl Default for ShowToolProposalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ShowToolProposalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ShowToolProposalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "show_tool_proposal".to_string(),
            description: "show detailed information about a tool proposal".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool proposal to show (without .json extension)"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal = genesis.load_proposal(&name)?;
        let proposal_path = genesis.proposals_dir().join(format!("{}.json", name));

        let mut lines = Vec::new();
        lines.push(format!("Name: {}", proposal.name));
        lines.push(format!("Status: {}", proposal.status));
        lines.push(format!("Requirement: {}", proposal.requirement));
        lines.push(format!("Description: {}", proposal.description));
        lines.push(format!("Rationale: {}", proposal.rationale));

        if !proposal.inputs.is_empty() {
            lines.push("Inputs:".to_string());
            for input in &proposal.inputs {
                lines.push(format!(
                    "  - {}: {} ({})",
                    input.name,
                    input.description,
                    if input.required {
                        "required"
                    } else {
                        "optional"
                    }
                ));
            }
        }

        if !proposal.argv_template.is_empty() {
            lines.push(format!("Argv template: {:?}", proposal.argv_template));
        }

        lines.push("Verification:".to_string());
        lines.push(format!(
            "  Args: {:?}",
            proposal.verification.verification_args
        ));
        lines.push(format!(
            "  Expected exit: {}",
            proposal.verification.expected_exit
        ));
        if let Some(expected) = &proposal.verification.expected_output_contains {
            lines.push(format!("  Expected output contains: {}", expected));
        }

        lines.push(format!("Created at: {}", proposal.created_at));
        if let Some(approved_at) = &proposal.approved_at {
            lines.push(format!("Approved at: {}", approved_at));
        }
        if let Some(rejected_at) = &proposal.rejected_at {
            lines.push(format!("Rejected at: {}", rejected_at));
        }
        if let Some(reason) = &proposal.reason {
            lines.push(format!("Reason: {}", reason));
        }
        if let Some(revised_at) = &proposal.revised_at {
            lines.push(format!("Revised at: {}", revised_at));
        }

        lines.push(format!("\nFile: {}", proposal_path.display()));
        Ok(lines.join("\n"))
    }
}

pub struct ApproveToolProposalTool;

impl Default for ApproveToolProposalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ApproveToolProposalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ApproveToolProposalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "approve_tool_proposal".to_string(),
            description: "approve a proposed tool design for implementation; only approved proposals can be implemented"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool proposal to approve (without .json extension)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "optional reason for approval"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let reason = get_optional_string(&args, "reason");
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal = genesis.load_proposal(&name)?;

        match proposal.status {
            ProposalStatus::Proposed => {
                genesis.update_proposal_metadata(
                    &name,
                    ProposalStatus::Approved,
                    Some(get_current_timestamp()),
                    None,
                    reason,
                    None,
                )?;
                Ok(format!("proposal '{}' approved for implementation", name))
            }
            ProposalStatus::Approved => Err(Error::InvalidInput(format!(
                "proposal '{}' is already approved",
                name
            ))),
            ProposalStatus::Implemented => Err(Error::InvalidInput(format!(
                "proposal '{}' has already been implemented",
                name
            ))),
            ProposalStatus::Verified => Err(Error::InvalidInput(format!(
                "proposal '{}' has already been verified",
                name
            ))),
            ProposalStatus::Rejected => Err(Error::InvalidInput(format!(
                "proposal '{}' was rejected",
                name
            ))),
        }
    }
}

pub struct RejectToolProposalTool;

impl Default for RejectToolProposalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RejectToolProposalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for RejectToolProposalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "reject_tool_proposal".to_string(),
            description: "reject a proposed tool design; rejected proposals cannot be implemented"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool proposal to reject (without .json extension)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "optional reason for rejection"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let reason = get_optional_string(&args, "reason");
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal = genesis.load_proposal(&name)?;

        match proposal.status {
            ProposalStatus::Proposed | ProposalStatus::Approved => {
                genesis.update_proposal_metadata(
                    &name,
                    ProposalStatus::Rejected,
                    None,
                    Some(get_current_timestamp()),
                    reason,
                    None,
                )?;
                Ok(format!("proposal '{}' rejected", name))
            }
            ProposalStatus::Rejected => Err(Error::InvalidInput(format!(
                "proposal '{}' is already rejected",
                name
            ))),
            ProposalStatus::Implemented => Err(Error::InvalidInput(format!(
                "proposal '{}' has already been implemented",
                name
            ))),
            ProposalStatus::Verified => Err(Error::InvalidInput(format!(
                "proposal '{}' has already been verified",
                name
            ))),
        }
    }
}

pub struct ReviseToolProposalTool;

impl Default for ReviseToolProposalTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReviseToolProposalTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ReviseToolProposalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "revise_tool_proposal".to_string(),
            description: "revise a proposed tool design before implementation; allowed for proposed and approved proposals; revising an approved proposal resets status to proposed"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool proposal to revise (without .json extension)"
                    },
                    "description": {
                        "type": "string",
                        "description": "updated description"
                    },
                    "inputs": {
                        "type": "array",
                        "description": "updated inputs array"
                    },
                    "argv_template": {
                        "type": "array",
                        "description": "updated argv template"
                    },
                    "verification_args": {
                        "type": "array",
                        "description": "updated verification args"
                    },
                    "expected_exit": {
                        "type": "integer",
                        "description": "updated expected exit code"
                    },
                    "expected_output_contains": {
                        "type": "string",
                        "description": "updated expected output substring"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let proposal = genesis.load_proposal(&name)?;
        let original_status = proposal.status;

        match original_status {
            ProposalStatus::Proposed | ProposalStatus::Approved => {}
            ProposalStatus::Implemented => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' has already been implemented; cannot revise",
                    name
                )));
            }
            ProposalStatus::Verified => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' has already been verified; cannot revise",
                    name
                )));
            }
            ProposalStatus::Rejected => {
                return Err(Error::InvalidInput(format!(
                    "proposal '{}' was rejected; cannot revise",
                    name
                )));
            }
        }

        let mut updated_proposal = proposal;
        updated_proposal.revised_at = Some(get_current_timestamp());

        if let Some(description) = get_optional_string(&args, "description") {
            updated_proposal.description = description;
        }
        if args
            .get("inputs")
            .and_then(|value| value.as_array())
            .is_some()
        {
            updated_proposal.inputs = parse_inputs(&args);
        }
        if args
            .get("argv_template")
            .and_then(|value| value.as_array())
            .is_some()
        {
            updated_proposal.argv_template = parse_string_array(&args, "argv_template");
        }
        if args
            .get("verification_args")
            .and_then(|value| value.as_array())
            .is_some()
        {
            updated_proposal.verification.verification_args =
                parse_string_array(&args, "verification_args");
        }
        if let Some(expected_exit) = args.get("expected_exit").and_then(|value| value.as_i64()) {
            updated_proposal.verification.expected_exit = expected_exit as i32;
        }
        if let Some(expected_output) = get_optional_string(&args, "expected_output_contains") {
            updated_proposal.verification.expected_output_contains = Some(expected_output);
        }

        if original_status == ProposalStatus::Approved {
            updated_proposal.status = ProposalStatus::Proposed;
            updated_proposal.approved_at = None;
        }

        genesis.save_proposal(&updated_proposal)?;

        let status_note = if original_status == ProposalStatus::Approved {
            "; status reset to proposed"
        } else {
            ""
        };
        Ok(format!(
            "proposal '{}' revised successfully{}",
            name, status_note
        ))
    }
}

fn parse_inputs(args: &serde_json::Value) -> Vec<ToolInput> {
    args.get("inputs")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_string_array(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

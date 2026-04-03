use super::{get_string, ToolGenesis, ToolInput, VerificationSpec};
use crate::context::ToolContext;
use crate::error::Error;
use crate::tool_spec::ToolSpec;
use crate::tools::Tool;
use crate::Result;

pub struct ListGeneratedToolsTool;

impl Default for ListGeneratedToolsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ListGeneratedToolsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ListGeneratedToolsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_generated_tools".to_string(),
            description: "list workspace-local generated tools in .topagent/tools/ with their name, description, and verification status".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn execute(&self, _args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let tools = genesis.list_generated_tools()?;
        if tools.is_empty() {
            return Ok("no generated tools found in .topagent/tools/".to_string());
        }

        let lines: Vec<_> = tools
            .into_iter()
            .map(|tool| {
                let status = if tool.verified {
                    "[verified]"
                } else {
                    "[unverified]"
                };
                format!("- {} {}: {}", status, tool.name, tool.description)
            })
            .collect();
        Ok(lines.join("\n"))
    }
}

pub struct DeleteGeneratedToolTool;

impl Default for DeleteGeneratedToolTool {
    fn default() -> Self {
        Self::new()
    }
}

impl DeleteGeneratedToolTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for DeleteGeneratedToolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_generated_tool".to_string(),
            description: "delete a generated tool from .topagent/tools/; the tool is disposed and can be recreated if needed"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the generated tool to delete"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        genesis.delete_generated_tool(&name)?;
        Ok(format!("tool '{}' deleted from .topagent/tools/", name))
    }
}

pub struct CreateToolTool;

impl Default for CreateToolTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateToolTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for CreateToolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_tool".to_string(),
            description: "create a workspace-local tool with verification; tools are disposable and can be deleted and recreated easily".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "requirement": {
                        "type": "string",
                        "description": "what the tool should accomplish (for record keeping)"
                    },
                    "name": {
                        "type": "string",
                        "description": "unique name for the tool (alphanumeric + underscore only)"
                    },
                    "description": {
                        "type": "string",
                        "description": "human-readable description of what the tool does"
                    },
                    "script": {
                        "type": "string",
                        "description": "shell script content (executable commands)"
                    },
                    "inputs": {
                        "type": "array",
                        "description": "named string inputs for the tool",
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
                        "description": "arguments to pass to the generated script during verification, e.g. ['--test']",
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
                "required": ["requirement", "name", "description", "script", "verification_args"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let requirement = get_string(&args, "requirement")?;
        let name = get_string(&args, "name")?;
        let description = get_string(&args, "description")?;
        let script = get_string(&args, "script")?;

        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(Error::InvalidInput(
                "tool name must be alphanumeric + underscore only".to_string(),
            ));
        }

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let result = genesis.create_tool(
            &requirement,
            &name,
            &description,
            &script,
            parse_inputs(&args),
            parse_string_array(&args, "argv_template"),
            Some(parse_verification_spec(&args)),
        )?;

        if result.success {
            let script_path = genesis.tools_dir().join(&name).join("script.sh");
            Ok(format!(
                "tool '{}' created and verified successfully\npath: {}",
                result.tool_name,
                script_path.display()
            ))
        } else {
            Ok(format!(
                "tool '{}' created but verification failed: {}\n\
                 use repair_tool to fix and re-verify",
                result.tool_name, result.message
            ))
        }
    }
}

pub struct RepairToolTool;

impl Default for RepairToolTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RepairToolTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for RepairToolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "repair_tool".to_string(),
            description:
                "repair a failed generated tool by providing a new script and re-verify once; if verification still fails, call repair_tool again with corrected inputs"
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "name of the tool to repair"
                    },
                    "script": {
                        "type": "string",
                        "description": "updated shell script content"
                    },
                    "inputs": {
                        "type": "array",
                        "description": "optional named inputs for the tool",
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
                        "description": "optional command argv template with {var_name} placeholders"
                    },
                    "verification_args": {
                        "type": "array",
                        "description": "arguments to pass to the script during verification",
                        "items": { "type": "string" }
                    },
                    "expected_exit": {
                        "type": "integer",
                        "description": "expected exit code (default: 0)"
                    },
                    "expected_output_contains": {
                        "type": "string",
                        "description": "optional string that must appear in output"
                    }
                },
                "required": ["name", "script", "verification_args"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let script = get_string(&args, "script")?;

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let inputs = args
            .get("inputs")
            .and_then(|value| value.as_array())
            .map(|_| parse_inputs(&args));
        let argv_template = args
            .get("argv_template")
            .and_then(|value| value.as_array())
            .map(|_| parse_string_array(&args, "argv_template"));

        let result = genesis.repair_tool(
            &name,
            &script,
            inputs,
            argv_template,
            Some(&parse_verification_spec(&args)),
        )?;

        if result.success {
            Ok(format!(
                "tool '{}' repaired and verified successfully",
                result.tool_name
            ))
        } else {
            Err(Error::ToolFailed(format!(
                "tool '{}' still failing after repair: {}\ncall repair_tool again with corrected script",
                result.tool_name, result.message
            )))
        }
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

fn parse_verification_spec(args: &serde_json::Value) -> VerificationSpec {
    VerificationSpec {
        verification_args: parse_string_array(args, "verification_args"),
        expected_exit: args
            .get("expected_exit")
            .and_then(|value| value.as_i64())
            .unwrap_or(0) as i32,
        expected_output_contains: args
            .get("expected_output_contains")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(String::from),
    }
}

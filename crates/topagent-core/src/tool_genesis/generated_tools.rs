use super::{get_string, validate_tool_name, ToolGenesis, ToolInput, VerificationSpec};
use crate::context::ToolContext;
use crate::error::Error;
use crate::tool_spec::ToolSpec;
use crate::tools::Tool;
use crate::Result;
use std::collections::BTreeMap;

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
                let status = if tool.load_warning.is_some() {
                    "[unavailable]"
                } else if tool.verified {
                    "[verified]"
                } else {
                    "[unverified]"
                };
                match tool.load_warning {
                    Some(warning) => {
                        format!(
                            "- {} {}: {} ({})",
                            status, tool.name, tool.description, warning
                        )
                    }
                    None => format!("- {} {}: {}", status, tool.name, tool.description),
                }
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
                        "description": "legacy positional verification values; for tools with named inputs, values are matched in declared input order",
                        "items": { "type": "string" }
                    },
                    "verification_inputs": {
                        "type": "object",
                        "description": "named verification inputs that use the same contract as runtime tool execution",
                        "additionalProperties": { "type": "string" }
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
                "required": ["name", "description", "script"],
                "anyOf": [
                    { "required": ["verification_args"] },
                    { "required": ["verification_inputs"] }
                ]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let description = get_string(&args, "description")?;
        let script = get_string(&args, "script")?;

        validate_tool_name(&name)?;

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let result = genesis.create_tool(
            &name,
            &description,
            &script,
            parse_inputs(&args)?,
            parse_string_array(&args, "argv_template")?,
            Some(parse_verification_spec(&args)?),
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
                        "description": "legacy positional verification values; for tools with named inputs, values are matched in declared input order",
                        "items": { "type": "string" }
                    },
                    "verification_inputs": {
                        "type": "object",
                        "description": "named verification inputs that use the same contract as runtime tool execution",
                        "additionalProperties": { "type": "string" }
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
                "required": ["name", "script"],
                "anyOf": [
                    { "required": ["verification_args"] },
                    { "required": ["verification_inputs"] }
                ]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let name = get_string(&args, "name")?;
        let script = get_string(&args, "script")?;
        validate_tool_name(&name)?;

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());
        let inputs = args
            .get("inputs")
            .and_then(|value| value.as_array())
            .map(|_| parse_inputs(&args))
            .transpose()?;
        let argv_template = args
            .get("argv_template")
            .and_then(|value| value.as_array())
            .map(|_| parse_string_array(&args, "argv_template"))
            .transpose()?;

        let result = genesis.repair_tool(
            &name,
            &script,
            inputs,
            argv_template,
            Some(&parse_verification_spec(&args)?),
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

fn parse_inputs(args: &serde_json::Value) -> Result<Vec<ToolInput>> {
    let Some(items) = args.get("inputs") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| Error::InvalidInput("missing or invalid 'inputs' field".to_string()))?;

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            serde_json::from_value(item.clone())
                .map_err(|err| Error::InvalidInput(format!("invalid inputs[{}]: {}", index, err)))
        })
        .collect()
}

fn parse_string_array(args: &serde_json::Value, key: &str) -> Result<Vec<String>> {
    let Some(items) = args.get(key) else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))?;

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(String::from)
                .ok_or_else(|| Error::InvalidInput(format!("{}[{}] must be a string", key, index)))
        })
        .collect()
}

fn parse_verification_spec(args: &serde_json::Value) -> Result<VerificationSpec> {
    Ok(VerificationSpec {
        verification_inputs: parse_string_map(args, "verification_inputs")?,
        verification_args: parse_string_array(args, "verification_args")?,
        expected_exit: args
            .get("expected_exit")
            .and_then(|value| value.as_i64())
            .unwrap_or(0) as i32,
        expected_output_contains: args
            .get("expected_output_contains")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(String::from),
    })
}

fn parse_string_map(args: &serde_json::Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(obj) = args.get(key) else {
        return Ok(BTreeMap::new());
    };
    let obj = obj
        .as_object()
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))?;

    let mut values = BTreeMap::new();
    for (map_key, value) in obj {
        let value = value.as_str().ok_or_else(|| {
            Error::InvalidInput(format!(
                "verification input '{}' in '{}' must be a string",
                map_key, key
            ))
        })?;
        values.insert(map_key.clone(), value.to_string());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ExecutionContext;
    use crate::runtime::RuntimeOptions;
    use tempfile::TempDir;

    #[test]
    fn test_create_tool_rejects_invalid_inputs_payload() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = CreateToolTool::new();

        let result = tool.execute(
            serde_json::json!({
                "name": "bad_inputs",
                "description": "broken",
                "script": "echo ok",
                "inputs": ["not an object"],
                "verification_args": []
            }),
            &ctx,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid inputs[0]"));
    }

    #[test]
    fn test_create_tool_rejects_non_string_argv_template() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = CreateToolTool::new();

        let result = tool.execute(
            serde_json::json!({
                "name": "bad_argv",
                "description": "broken",
                "script": "echo ok",
                "argv_template": [1],
                "verification_args": []
            }),
            &ctx,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("argv_template[0] must be a string"));
    }

    #[test]
    fn test_list_generated_tools_surfaces_unavailable_reason() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "broken_after_verify",
                "broken after verify",
                "echo ok",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("ok".to_string()),
                }),
            )
            .unwrap();
        std::fs::remove_file(
            temp.path()
                .join(".topagent/tools/broken_after_verify/script.sh"),
        )
        .unwrap();

        let tool = ListGeneratedToolsTool::new();
        let output = tool.execute(serde_json::json!({}), &ctx).unwrap();

        assert!(output.contains("[unavailable] broken_after_verify"));
        assert!(output.contains("missing script.sh"));
    }
}

use crate::context::ToolContext;
use crate::error::Error;
use crate::external::ExternalTool;
use crate::tool_spec::ToolSpec;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

const TOOLS_DIR: &str = ".rust-pi/tools";
const GENESIS_DIR: &str = ".rust-pi/tool-genesis";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args_template: Option<String>,
    pub verification: Option<VerificationSpec>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub inputs: Vec<ToolInput>,
    #[serde(default)]
    pub argv_template: Vec<String>,
    #[serde(default)]
    pub manifest_version: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSpec {
    pub verification_args: Vec<String>,
    #[serde(default)]
    pub expected_exit: i32,
    #[serde(default)]
    pub expected_output_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisRecord {
    pub requirement: String,
    pub tool_name: String,
    pub verification_passed: bool,
    pub repair_attempts: usize,
    pub final_outcome: String,
    pub created_at: String,
}

pub struct ToolGenesis {
    workspace_root: PathBuf,
}

impl ToolGenesis {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    pub fn tools_dir(&self) -> PathBuf {
        self.workspace_root.join(TOOLS_DIR)
    }

    pub fn genesis_dir(&self) -> PathBuf {
        self.workspace_root.join(GENESIS_DIR)
    }

    pub fn create_tool(
        &self,
        requirement: &str,
        name: &str,
        description: &str,
        command: &str,
        args_template: Option<&str>,
        inputs: Vec<ToolInput>,
        argv_template: Vec<String>,
        verification: Option<VerificationSpec>,
    ) -> Result<GenesisResult> {
        let tool_dir = self.tools_dir().join(name);
        let manifest_path = tool_dir.join("manifest.json");

        if manifest_path.exists() {
            return Ok(GenesisResult {
                success: false,
                tool_name: name.to_string(),
                message: format!(
                    "tool '{}' already exists at {}",
                    name,
                    manifest_path.display()
                ),
                verification_passed: false,
                repair_attempts: 0,
            });
        }

        if !inputs.is_empty() && argv_template.is_empty() {
            return Err(Error::InvalidInput(
                "if inputs are defined, argv_template must also be defined".to_string(),
            ));
        }

        std::fs::create_dir_all(&tool_dir)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

        let script_path = tool_dir.join("script.sh");
        std::fs::write(&script_path, command).map_err(Error::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        }

        let manifest = ToolManifest {
            name: name.to_string(),
            description: description.to_string(),
            command: command.to_string(),
            args_template: args_template.map(String::from),
            verification: verification.clone(),
            verified: false,
            inputs,
            argv_template,
            manifest_version: Some(1),
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        std::fs::write(&manifest_path, &manifest_json).map_err(Error::Io)?;

        let verify_result = if let Some(v) = &verification {
            let verified = self.verify_tool(name, v)?;
            if verified {
                let mut m = manifest;
                m.verified = true;
                let updated = serde_json::to_string_pretty(&m)
                    .map_err(|e| Error::InvalidInput(e.to_string()))?;
                std::fs::write(&manifest_path, updated).map_err(Error::Io)?;
            }
            verified
        } else {
            false
        };

        let outcome = if verify_result {
            "promoted".to_string()
        } else if verification.is_some() {
            "failed_verification".to_string()
        } else {
            "awaiting_verification".to_string()
        };

        let record = GenesisRecord {
            requirement: requirement.to_string(),
            tool_name: name.to_string(),
            verification_passed: verify_result,
            repair_attempts: 0,
            final_outcome: outcome.clone(),
            created_at: chrono_timestamp(),
        };
        self.write_genesis_record(&record)?;

        Ok(GenesisResult {
            success: verify_result || verification.is_none(),
            tool_name: name.to_string(),
            message: if verify_result {
                format!("tool '{}' created and verified", name)
            } else if verification.is_some() {
                format!("tool '{}' created but verification failed", name)
            } else {
                format!("tool '{}' created (no verification provided)", name)
            },
            verification_passed: verify_result,
            repair_attempts: 0,
        })
    }

    pub fn repair_tool(
        &self,
        name: &str,
        new_command: &str,
        new_args_template: Option<&str>,
        new_inputs: Option<Vec<ToolInput>>,
        new_argv_template: Option<Vec<String>>,
        new_verification: Option<&VerificationSpec>,
    ) -> Result<GenesisResult> {
        let tool_dir = self.tools_dir().join(name);
        let manifest_path = tool_dir.join("manifest.json");

        if !manifest_path.exists() {
            return Err(Error::InvalidInput(format!(
                "tool '{}' does not exist",
                name
            )));
        }

        let content = std::fs::read_to_string(&manifest_path).map_err(Error::Io)?;
        let mut manifest: ToolManifest =
            serde_json::from_str(&content).map_err(|e| Error::InvalidInput(e.to_string()))?;

        manifest.command = new_command.to_string();
        manifest.args_template = new_args_template.map(String::from);
        if let Some(inputs) = new_inputs {
            manifest.inputs = inputs;
        }
        if let Some(argv) = new_argv_template {
            manifest.argv_template = argv;
        }
        if let Some(v) = new_verification {
            manifest.verification = Some(v.clone());
        }
        manifest.verified = false;
        manifest.manifest_version = Some(1);

        let script_path = tool_dir.join("script.sh");
        std::fs::write(&script_path, new_command).map_err(Error::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        }

        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        std::fs::write(&manifest_path, &manifest_json).map_err(Error::Io)?;

        let verify_result = if let Some(v) = manifest.verification.clone() {
            self.verify_tool(name, &v)?
        } else {
            false
        };

        if verify_result {
            manifest.verified = true;
            let updated = serde_json::to_string_pretty(&manifest)
                .map_err(|e| Error::InvalidInput(e.to_string()))?;
            std::fs::write(&manifest_path, updated).map_err(Error::Io)?;
        }

        Ok(GenesisResult {
            success: verify_result,
            tool_name: name.to_string(),
            message: if verify_result {
                format!("tool '{}' repaired and verified", name)
            } else {
                format!(
                    "tool '{}' repair attempted but verification still failing",
                    name
                )
            },
            verification_passed: verify_result,
            repair_attempts: 1,
        })
    }

    pub fn verify_tool(&self, name: &str, spec: &VerificationSpec) -> Result<bool> {
        let tool_dir = self.tools_dir().join(name);
        let script_path = tool_dir.join("script.sh");

        if !script_path.exists() {
            return Ok(false);
        }

        let mut cmd = Command::new("sh");
        cmd.current_dir(&self.workspace_root);
        cmd.arg(&script_path);
        for arg in &spec.verification_args {
            cmd.arg(arg);
        }

        let output = cmd.output().map_err(|e| {
            Error::ToolFailed(format!(
                "verification failed to run generated script: {}",
                e
            ))
        })?;

        let exit_match = output.status.code() == Some(spec.expected_exit);
        let output_contains_match = if let Some(ref expected) = spec.expected_output_contains {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            stdout.contains(expected) || stderr.contains(expected)
        } else {
            true
        };

        Ok(exit_match && output_contains_match)
    }

    pub fn load_verified_tools(&self) -> Result<Vec<ExternalTool>> {
        let tools_dir = self.tools_dir();
        if !tools_dir.exists() {
            return Ok(Vec::new());
        }

        let mut tools = Vec::new();
        for entry in std::fs::read_dir(tools_dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(&manifest_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let manifest: ToolManifest = match serde_json::from_str(&content) {
                Ok(m) => m,
                Err(_) => continue,
            };

            if !manifest.verified {
                continue;
            }

            let script_path = path.join("script.sh");
            if !script_path.exists() {
                continue;
            }

            let args_template = if !manifest.argv_template.is_empty() {
                build_args_template_from_argv(&script_path, &manifest.argv_template)
            } else if let Some(ref tmpl) = manifest.args_template {
                format!("{} {}", script_path.display(), tmpl)
            } else {
                format!("{} $@", script_path.display())
            };

            let input_schema = if !manifest.inputs.is_empty() {
                build_input_schema(&manifest.inputs)
            } else {
                serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })
            };

            let tool = ExternalTool::new(&manifest.name, &manifest.description, "sh")
                .with_args_template(&args_template)
                .with_input_schema(input_schema);
            tools.push(tool);
        }
        Ok(tools)
    }

    pub fn list_generated_tools(&self) -> Result<Vec<ToolManifest>> {
        let tools_dir = self.tools_dir();
        if !tools_dir.exists() {
            return Ok(Vec::new());
        }

        let mut manifests = Vec::new();
        for entry in std::fs::read_dir(tools_dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&manifest_path).map_err(Error::Io)?;
            let manifest: ToolManifest =
                serde_json::from_str(&content).map_err(|e| Error::InvalidInput(e.to_string()))?;
            manifests.push(manifest);
        }
        Ok(manifests)
    }

    fn write_genesis_record(&self, record: &GenesisRecord) -> Result<()> {
        let genesis_dir = self.genesis_dir();
        std::fs::create_dir_all(&genesis_dir)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

        let filename = format!(
            "{}-{}.json",
            timestamp_for_filename(&record.created_at),
            sanitize_filename(&record.tool_name)
        );
        let path = genesis_dir.join(filename);
        let json =
            serde_json::to_string_pretty(record).map_err(|e| Error::InvalidInput(e.to_string()))?;
        std::fs::write(&path, json).map_err(Error::Io)?;
        Ok(())
    }

    pub fn list_genesis_records(&self) -> Result<Vec<GenesisRecord>> {
        let genesis_dir = self.genesis_dir();
        if !genesis_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for entry in std::fs::read_dir(genesis_dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let content = std::fs::read_to_string(&path).map_err(Error::Io)?;
            let record: GenesisRecord =
                serde_json::from_str(&content).map_err(|e| Error::InvalidInput(e.to_string()))?;
            records.push(record);
        }
        Ok(records)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisResult {
    pub success: bool,
    pub tool_name: String,
    pub message: String,
    pub verification_passed: bool,
    pub repair_attempts: usize,
}

fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let nanos = dur.subsec_nanos();
    format!("{}.{:09}", secs, nanos)
}

fn timestamp_for_filename(ts: &str) -> String {
    ts.replace(".", "-").replace(":", "-")
}

fn sanitize_filename(name: &str) -> String {
    name.replace("/", "_").replace(" ", "_")
}

fn build_args_template_from_argv(
    script_path: &std::path::Path,
    argv_template: &[String],
) -> String {
    let mut parts = Vec::new();
    parts.push(script_path.display().to_string());
    for arg in argv_template {
        if arg.starts_with('{') && arg.ends_with('}') {
            let var_name = &arg[1..arg.len() - 1];
            parts.push(format!("{{{}}}", var_name));
        } else {
            parts.push(arg.clone());
        }
    }
    parts.join(" ")
}

fn build_input_schema(inputs: &[ToolInput]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for input in inputs {
        properties.insert(
            input.name.clone(),
            serde_json::json!({
                "type": "string",
                "description": input.description
            }),
        );
        if input.required {
            required.push(input.name.clone());
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

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
            description: "list all generated tools in .rust-pi/tools/ with their name, description, and verification status".to_string(),
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
            return Ok("no generated tools found in .rust-pi/tools/".to_string());
        }
        let mut lines = Vec::new();
        for t in tools {
            let status = if t.verified {
                "[verified]"
            } else {
                "[unverified]"
            };
            lines.push(format!("- {} {}: {}", status, t.name, t.description));
        }
        Ok(lines.join("\n"))
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
            description: "create a new reusable workspace-local tool with verification; tools are stored in .rust-pi/tools/ and only become available after passing verification".to_string(),
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
                    "args_template": {
                        "type": "string",
                        "description": "optional legacy shell argument template using {arg_name} placeholders, e.g. '{input} --flag'",
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
        let args_template = args
            .get("args_template")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let inputs: Vec<ToolInput> = args
            .get("inputs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let obj = item.as_object()?;
                        let name = obj.get("name")?.as_str()?.to_string();
                        let description = obj.get("description")?.as_str()?.to_string();
                        let required = obj
                            .get("required")
                            .and_then(|r| r.as_bool())
                            .unwrap_or(true);
                        Some(ToolInput {
                            name,
                            description,
                            required,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let argv_template: Vec<String> = args
            .get("argv_template")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let verification_args: Vec<String> = args
            .get("verification_args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let expected_exit = args
            .get("expected_exit")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let expected_output_contains = args
            .get("expected_output_contains")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(Error::InvalidInput(
                "tool name must be alphanumeric + underscore only".to_string(),
            ));
        }

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());

        let verification = Some(VerificationSpec {
            verification_args,
            expected_exit,
            expected_output_contains,
        });

        let result = genesis.create_tool(
            &requirement,
            &name,
            &description,
            &script,
            args_template,
            inputs,
            argv_template,
            verification,
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
                    "args_template": {
                        "type": "string",
                        "description": "optional updated argument template"
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
        let args_template = args
            .get("args_template")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let inputs: Option<Vec<ToolInput>> =
            args.get("inputs").and_then(|v| v.as_array()).map(|arr| {
                let mut inputs = Vec::new();
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        if let (Some(n), Some(d)) = (
                            obj.get("name").and_then(|n| n.as_str()),
                            obj.get("description").and_then(|d| d.as_str()),
                        ) {
                            let required = obj
                                .get("required")
                                .and_then(|r| r.as_bool())
                                .unwrap_or(true);
                            inputs.push(ToolInput {
                                name: n.to_string(),
                                description: d.to_string(),
                                required,
                            });
                        }
                    }
                }
                inputs
            });

        let argv_template: Option<Vec<String>> = args
            .get("argv_template")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            });

        let verification_args: Vec<String> = args
            .get("verification_args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let expected_exit = args
            .get("expected_exit")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let expected_output_contains = args
            .get("expected_output_contains")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        let genesis = ToolGenesis::new(ctx.exec.workspace_root.clone());

        let verification = Some(VerificationSpec {
            verification_args,
            expected_exit,
            expected_output_contains,
        });

        let result = genesis.repair_tool(
            &name,
            &script,
            args_template,
            inputs,
            argv_template,
            verification.as_ref(),
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

fn get_string(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))
}

use crate::tools::Tool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use tempfile::TempDir;

    #[test]
    fn test_tool_genesis_create_and_verify() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "echo hello",
                "echo_hello",
                "echo hello",
                "echo hello",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec!["hello".to_string()],
                    expected_exit: 0,
                    expected_output_contains: Some("hello".to_string()),
                }),
            )
            .unwrap();

        assert!(result.success);
        assert_eq!(result.tool_name, "echo_hello");
        assert!(result.verification_passed);

        let tools = genesis.load_verified_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name, "echo_hello");
    }

    #[test]
    fn test_tool_genesis_fails_verification() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "fail",
                "failing_tool",
                "fails",
                "exit 1",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: None,
                }),
            )
            .unwrap();

        assert!(!result.success);
        assert!(!result.verification_passed);
    }

    #[test]
    fn test_list_generated_tools() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "test",
                "tool_one",
                "first",
                "echo one",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("one".to_string()),
                }),
            )
            .unwrap();

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "tool_one");
    }

    #[test]
    fn test_genesis_record_written() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "test record",
                "record_test",
                "test",
                "echo ok",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("ok".to_string()),
                }),
            )
            .unwrap();

        let records = genesis.list_genesis_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "record_test");
        assert_eq!(records[0].requirement, "test record");
    }

    #[test]
    fn test_create_tool_name_validation() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = CreateToolTool::new();

        let bad_args = serde_json::json!({
            "requirement": "test",
            "name": "bad/name",
            "description": "test",
            "script": "echo hi",
            "verification_args": <Vec<String>>::new()
        });

        let result = tool.execute(bad_args, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_and_repair_tool() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "test",
                "repairable",
                "initially broken",
                "exit 1",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: None,
                }),
            )
            .unwrap();

        assert!(!result.success);

        let repair_result = genesis
            .repair_tool(
                "repairable",
                "echo fixed",
                None,
                None,
                None,
                Some(&VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("fixed".to_string()),
                }),
            )
            .unwrap();

        assert!(repair_result.success);
        assert_eq!(repair_result.repair_attempts, 1);
    }

    #[test]
    fn test_verification_invokes_generated_script() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let script = "echo SCRIPT_OUTPUT";
        let result = genesis
            .create_tool(
                "verify script was run",
                "script_check",
                "checks script runs",
                script,
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("SCRIPT_OUTPUT".to_string()),
                }),
            )
            .unwrap();

        assert!(
            result.success,
            "verification should pass when script output matches"
        );
        assert!(result.verification_passed);
    }

    #[test]
    fn test_incomplete_artifact_not_promoted() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "test",
                "incomplete_tool",
                "incomplete",
                "echo test",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("test".to_string()),
                }),
            )
            .unwrap();

        let tool_dir = temp.path().join(".rust-pi/tools/incomplete_tool");
        let manifest_path = tool_dir.join("manifest.json");
        let script_path = tool_dir.join("script.sh");

        let mut manifest: ToolManifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest.verified = true;
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(&script_path).unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(
            loaded.is_empty(),
            "tool with missing script should not be loaded even if manifest says verified"
        );
    }

    #[test]
    fn test_verification_fails_for_wrong_output() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "test wrong output",
                "wrong_output",
                "fails output check",
                "echo ACTUAL_OUTPUT",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("WRONG_OUTPUT".to_string()),
                }),
            )
            .unwrap();

        assert!(
            !result.success,
            "verification should fail when output does not match"
        );
        assert!(!result.verification_passed);
    }

    #[test]
    fn test_repair_is_single_step() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "test",
                "single_repair",
                "broken",
                "exit 1",
                None,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: None,
                }),
            )
            .unwrap();

        let repair_result = genesis
            .repair_tool(
                "single_repair",
                "exit 1",
                None,
                None,
                None,
                Some(&VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: None,
                }),
            )
            .unwrap();

        assert!(
            !repair_result.success,
            "second repair step should still fail for same broken script"
        );
        assert_eq!(
            repair_result.repair_attempts, 1,
            "repair_attempts should be exactly 1 (manual single-step)"
        );
    }

    #[test]
    fn test_corrupt_manifest_not_loaded_as_verified() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let tool_dir = temp.path().join(".rust-pi/tools/bad_manifest");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("manifest.json"), "not valid json{{{").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(loaded.is_empty(), "corrupt manifest should not be loaded");
    }
}

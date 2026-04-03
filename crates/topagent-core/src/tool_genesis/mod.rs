use crate::error::Error;
use crate::external::{resolve_argv_template, ExternalTool};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const TOOLS_DIR: &str = ".topagent/tools";

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
    pub verification: Option<VerificationSpec>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub inputs: Vec<ToolInput>,
    pub argv_template: Vec<String>,
    #[serde(default)]
    pub manifest_version: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationSpec {
    #[serde(default)]
    pub verification_inputs: BTreeMap<String, String>,
    pub verification_args: Vec<String>,
    #[serde(default)]
    pub expected_exit: i32,
    #[serde(default)]
    pub expected_output_contains: Option<String>,
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

    #[allow(clippy::too_many_arguments)]
    pub fn create_tool(
        &self,
        name: &str,
        description: &str,
        command: &str,
        inputs: Vec<ToolInput>,
        argv_template: Vec<String>,
        verification: Option<VerificationSpec>,
    ) -> Result<GenesisResult> {
        validate_tool_name(name)?;
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

        let manifest = ToolManifest {
            name: name.to_string(),
            description: description.to_string(),
            verification: verification.clone(),
            verified: false,
            inputs,
            argv_template,
            manifest_version: Some(1),
        };
        validate_manifest_interface(&manifest)?;
        if let Some(spec) = verification.as_ref() {
            validate_verification_spec(&manifest, spec)?;
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
        new_inputs: Option<Vec<ToolInput>>,
        new_argv_template: Option<Vec<String>>,
        new_verification: Option<&VerificationSpec>,
    ) -> Result<GenesisResult> {
        validate_tool_name(name)?;
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
        validate_manifest_interface(&manifest)?;
        if let Some(spec) = manifest.verification.as_ref() {
            validate_verification_spec(&manifest, spec)?;
        }

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
        validate_tool_name(name)?;
        let tool_dir = self.tools_dir().join(name);
        let manifest_path = tool_dir.join("manifest.json");
        let script_path = tool_dir.join("script.sh");

        if !manifest_path.exists() || !script_path.exists() {
            return Ok(false);
        }

        let content = std::fs::read_to_string(&manifest_path).map_err(Error::Io)?;
        let manifest: ToolManifest =
            serde_json::from_str(&content).map_err(|e| Error::InvalidInput(e.to_string()))?;
        validate_manifest_interface(&manifest)?;
        let verification_argv = verification_command_argv(&manifest, &script_path, spec)?;

        let mut cmd = Command::new("sh");
        cmd.current_dir(&self.workspace_root);
        for arg in verification_argv {
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
        Ok(self.generated_tool_inventory()?.verified_tools)
    }

    pub fn list_generated_tools(&self) -> Result<Vec<GeneratedToolSummary>> {
        Ok(self.generated_tool_inventory()?.summaries)
    }

    pub fn generated_tool_inventory(&self) -> Result<GeneratedToolInventory> {
        let scanned = self.scan_generated_tools()?;
        let mut summaries = Vec::with_capacity(scanned.len());
        let mut verified_tools = Vec::new();

        for entry in scanned {
            summaries.push(entry.summary);
            if let Some(tool) = entry.external_tool {
                verified_tools.push(tool);
            }
        }

        Ok(GeneratedToolInventory {
            summaries,
            verified_tools,
        })
    }

    pub fn delete_generated_tool(&self, name: &str) -> Result<()> {
        validate_tool_name(name)?;
        let tool_dir = self.tools_dir().join(name);
        if !tool_dir.exists() {
            return Err(Error::InvalidInput(format!(
                "tool '{}' does not exist at {}",
                name,
                tool_dir.display()
            )));
        }
        std::fs::remove_dir_all(&tool_dir).map_err(Error::Io)?;
        Ok(())
    }

    fn scan_generated_tools(&self) -> Result<Vec<ScannedGeneratedTool>> {
        let tools_dir = self.tools_dir();
        if !tools_dir.exists() {
            return Ok(Vec::new());
        }

        let mut paths = Vec::new();
        for entry in std::fs::read_dir(tools_dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();
            if path.is_dir() {
                paths.push(path);
            }
        }
        paths.sort();

        Ok(paths
            .iter()
            .map(|path| self.scan_generated_tool(path))
            .collect())
    }

    fn scan_generated_tool(&self, path: &Path) -> ScannedGeneratedTool {
        let fallback_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            return ScannedGeneratedTool::invalid(
                fallback_name,
                "invalid generated tool artifact",
                "missing manifest.json".to_string(),
            );
        }

        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(content) => content,
            Err(err) => {
                return ScannedGeneratedTool::invalid(
                    fallback_name,
                    "invalid generated tool artifact",
                    format!("failed to read manifest.json: {}", err),
                );
            }
        };

        let manifest: ToolManifest = match serde_json::from_str(&content) {
            Ok(manifest) => manifest,
            Err(err) => {
                return ScannedGeneratedTool::invalid(
                    fallback_name,
                    "invalid generated tool artifact",
                    format!("invalid manifest.json: {}", err),
                );
            }
        };

        let script_path = path.join("script.sh");
        let load_warning = if manifest.manifest_version.is_none() {
            Some(
                "missing manifest_version; recreate or repair the tool to make it usable"
                    .to_string(),
            )
        } else if let Err(err) = validate_manifest_interface(&manifest) {
            Some(format!("invalid interface: {}", err))
        } else if !script_path.exists() {
            Some("missing script.sh".to_string())
        } else {
            None
        };

        let external_tool = if manifest.verified && load_warning.is_none() {
            Some(external_tool_from_manifest(&manifest, &script_path))
        } else {
            None
        };

        ScannedGeneratedTool {
            summary: GeneratedToolSummary {
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                verified: manifest.verified,
                load_warning,
            },
            external_tool,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedToolSummary {
    pub name: String,
    pub description: String,
    pub verified: bool,
    pub load_warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GeneratedToolInventory {
    pub summaries: Vec<GeneratedToolSummary>,
    pub verified_tools: Vec<ExternalTool>,
}

struct ScannedGeneratedTool {
    summary: GeneratedToolSummary,
    external_tool: Option<ExternalTool>,
}

impl ScannedGeneratedTool {
    fn invalid(name: String, description: &str, warning: String) -> Self {
        Self {
            summary: GeneratedToolSummary {
                name,
                description: description.to_string(),
                verified: false,
                load_warning: Some(warning),
            },
            external_tool: None,
        }
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

fn validate_tool_name(name: &str) -> Result<()> {
    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "tool name must be alphanumeric + underscore only".to_string(),
        ))
    }
}

fn validate_manifest_interface(manifest: &ToolManifest) -> Result<()> {
    validate_tool_name(&manifest.name)?;

    if !manifest.inputs.is_empty() && manifest.argv_template.is_empty() {
        return Err(Error::InvalidInput(
            "if inputs are defined, argv_template must also be defined".to_string(),
        ));
    }

    let mut input_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for input in &manifest.inputs {
        if !input_names.insert(input.name.as_str()) {
            return Err(Error::InvalidInput(format!(
                "duplicate input name '{}'",
                input.name
            )));
        }
    }

    for part in &manifest.argv_template {
        if part.starts_with('{') && part.ends_with('}') {
            let placeholder = &part[1..part.len() - 1];
            if placeholder.is_empty() {
                return Err(Error::InvalidInput(
                    "empty placeholder in argv_template".to_string(),
                ));
            }
            if !input_names.contains(placeholder) {
                return Err(Error::InvalidInput(format!(
                    "placeholder '{{{}}}' in argv_template has no matching input",
                    placeholder
                )));
            }
        }
    }

    Ok(())
}

fn validate_verification_spec(manifest: &ToolManifest, spec: &VerificationSpec) -> Result<()> {
    let _ = verification_invocation_for_manifest(manifest, spec)?;
    Ok(())
}

enum VerificationInvocation {
    RuntimePayload(serde_json::Value),
    LegacyPositional(Vec<String>),
}

fn verification_invocation_for_manifest(
    manifest: &ToolManifest,
    spec: &VerificationSpec,
) -> Result<VerificationInvocation> {
    if !spec.verification_inputs.is_empty() {
        let known_inputs: std::collections::HashSet<&str> = manifest
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect();
        for key in spec.verification_inputs.keys() {
            if !known_inputs.contains(key.as_str()) {
                return Err(Error::InvalidInput(format!(
                    "verification input '{}' does not match any declared tool input",
                    key
                )));
            }
        }

        let payload = serde_json::Value::Object(
            spec.verification_inputs
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                .collect(),
        );
        resolve_argv_template(&manifest.argv_template, &payload, &manifest.name)?;
        return Ok(VerificationInvocation::RuntimePayload(payload));
    }

    if !manifest.inputs.is_empty() || !manifest.argv_template.is_empty() {
        if manifest.inputs.is_empty() {
            if !spec.verification_args.is_empty() {
                return Err(Error::InvalidInput(format!(
                    "tool '{}' has no declared inputs; verification_args cannot add extra runtime arguments",
                    manifest.name
                )));
            }

            let payload = serde_json::json!({});
            resolve_argv_template(&manifest.argv_template, &payload, &manifest.name)?;
            return Ok(VerificationInvocation::RuntimePayload(payload));
        }

        if spec.verification_args.len() != manifest.inputs.len() {
            return Err(Error::InvalidInput(format!(
                "tool '{}' verification_args must provide exactly {} values to match declared inputs; use verification_inputs for named verification",
                manifest.name,
                manifest.inputs.len()
            )));
        }

        let payload = serde_json::Value::Object(
            manifest
                .inputs
                .iter()
                .zip(spec.verification_args.iter())
                .map(|(input, value)| {
                    (input.name.clone(), serde_json::Value::String(value.clone()))
                })
                .collect(),
        );
        resolve_argv_template(&manifest.argv_template, &payload, &manifest.name)?;
        return Ok(VerificationInvocation::RuntimePayload(payload));
    }

    Ok(VerificationInvocation::LegacyPositional(
        spec.verification_args.clone(),
    ))
}

fn verification_command_argv(
    manifest: &ToolManifest,
    script_path: &Path,
    spec: &VerificationSpec,
) -> Result<Vec<String>> {
    let mut argv = vec![script_path.display().to_string()];
    match verification_invocation_for_manifest(manifest, spec)? {
        VerificationInvocation::RuntimePayload(payload) => {
            argv.extend(resolve_argv_template(
                &manifest.argv_template,
                &payload,
                &manifest.name,
            )?);
        }
        VerificationInvocation::LegacyPositional(args) => argv.extend(args),
    }
    Ok(argv)
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

fn external_tool_from_manifest(manifest: &ToolManifest, script_path: &Path) -> ExternalTool {
    let mut full_argv = vec![script_path.display().to_string()];
    full_argv.extend(manifest.argv_template.clone());

    let input_schema = build_input_schema(&manifest.inputs);

    ExternalTool::new(&manifest.name, &manifest.description, "sh")
        .with_argv_template(full_argv)
        .with_input_schema(input_schema)
}

mod generated_tools;

pub use generated_tools::{
    CreateToolTool, DeleteGeneratedToolTool, ListGeneratedToolsTool, RepairToolTool,
};

fn get_string(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use crate::tools::Tool;
    use tempfile::TempDir;

    #[test]
    fn test_tool_genesis_create_and_verify() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "echo_hello",
                "echo hello",
                "echo hello",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                "failing_tool",
                "fails",
                "exit 1",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                "tool_one",
                "first",
                "echo one",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("one".to_string()),
                }),
            )
            .unwrap();

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "tool_one");
        assert!(tools[0].load_warning.is_none());
    }

    #[test]
    fn test_create_tool_persists_only_manifest_and_script() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "persisted_tool",
                "test persistence",
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

        let tool_dir = temp.path().join(".topagent/tools/persisted_tool");
        let manifest_json = std::fs::read_to_string(tool_dir.join("manifest.json")).unwrap();
        assert!(tool_dir.join("script.sh").exists());
        assert!(
            !manifest_json.contains("\"command\""),
            "generated tool manifests should not duplicate the shell script body"
        );
        assert!(
            !temp.path().join(".topagent/tool-genesis").exists(),
            "generated tool creation should not leave behind a second persistence tree"
        );
    }

    #[test]
    fn test_create_tool_name_validation() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = CreateToolTool::new();

        let bad_args = serde_json::json!({
            "name": "bad/name",
            "description": "test",
            "script": "echo hi",
            "verification_args": <Vec<String>>::new()
        });

        let result = tool.execute(bad_args, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_repair_tool_name_validation() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis.repair_tool(
            "../bad_name",
            "echo fixed",
            None,
            None,
            Some(&VerificationSpec {
                verification_inputs: BTreeMap::new(),
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            }),
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("tool name must be alphanumeric + underscore only"));
    }

    #[test]
    fn test_delete_tool_name_validation() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis.delete_generated_tool("../bad_name");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("tool name must be alphanumeric + underscore only"));
    }

    #[test]
    fn test_create_tool_rejects_invalid_interface_before_writing_files() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis.create_tool(
            "invalid_interface",
            "broken interface",
            "echo ok",
            vec![ToolInput {
                name: "msg".to_string(),
                description: "message".to_string(),
                required: true,
            }],
            vec!["{missing}".to_string()],
            Some(VerificationSpec {
                verification_inputs: BTreeMap::from([("msg".to_string(), "ok".to_string())]),
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("has no matching input"));
        assert!(
            !temp
                .path()
                .join(".topagent/tools/invalid_interface")
                .exists(),
            "invalid tools should fail before any on-disk artifact is created"
        );
    }

    #[test]
    fn test_structured_generated_tool_verifies_with_named_inputs() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "named_verify",
                "echo args tool",
                "printf 'hello %s' \"$1\"",
                vec![ToolInput {
                    name: "name".to_string(),
                    description: "name to greet".to_string(),
                    required: true,
                }],
                vec!["{name}".to_string()],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::from([(
                        "name".to_string(),
                        "world".to_string(),
                    )]),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("hello world".to_string()),
                }),
            )
            .unwrap();

        assert!(result.success);
        assert!(result.verification_passed);
    }

    #[test]
    fn test_create_and_repair_tool() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "repairable",
                "initially broken",
                "exit 1",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                Some(&VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                "script_check",
                "checks script runs",
                script,
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                "incomplete_tool",
                "incomplete",
                "echo test",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("test".to_string()),
                }),
            )
            .unwrap();

        let tool_dir = temp.path().join(".topagent/tools/incomplete_tool");
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

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "incomplete_tool");
        assert_eq!(tools[0].load_warning.as_deref(), Some("missing script.sh"));
    }

    #[test]
    fn test_verification_fails_for_wrong_output() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "wrong_output",
                "fails output check",
                "echo ACTUAL_OUTPUT",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                "single_repair",
                "broken",
                "exit 1",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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
                Some(&VerificationSpec {
                    verification_inputs: BTreeMap::new(),
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

        let tool_dir = temp.path().join(".topagent/tools/bad_manifest");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("manifest.json"), "not valid json{{{").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(loaded.is_empty(), "corrupt manifest should not be loaded");

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "bad_manifest");
        assert!(tools[0]
            .load_warning
            .as_deref()
            .unwrap_or_default()
            .contains("invalid manifest.json"));
    }

    #[test]
    fn test_structured_generated_tool_reusable_with_spaces() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "echo_args",
                "echo args tool",
                "echo \"$@\"",
                vec![ToolInput {
                    name: "msg".to_string(),
                    description: "message to echo".to_string(),
                    required: true,
                }],
                vec!["{msg}".to_string()],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec!["test".to_string()],
                    expected_exit: 0,
                    expected_output_contains: Some("test".to_string()),
                }),
            )
            .unwrap();

        assert!(
            result.success,
            "tool creation and verification should succeed"
        );

        let loaded = genesis.load_verified_tools().unwrap();
        assert_eq!(loaded.len(), 1);

        let tool = &loaded[0];
        assert_eq!(tool.spec().name, "echo_args");

        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": "hello world with spaces"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap().output;
        assert!(output.contains("hello world with spaces"));
    }

    #[test]
    fn test_structured_generated_tool_reusable_special_chars() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "printf_special",
                "printf tool",
                "printf '%s' \"$1\"",
                vec![ToolInput {
                    name: "arg".to_string(),
                    description: "argument".to_string(),
                    required: true,
                }],
                vec!["{arg}".to_string()],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec!["ok".to_string()],
                    expected_exit: 0,
                    expected_output_contains: Some("ok".to_string()),
                }),
            )
            .unwrap();

        assert!(result.success);

        let loaded = genesis.load_verified_tools().unwrap();
        assert_eq!(loaded.len(), 1);

        let tool = &loaded[0];
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"arg": "$HOME/foo --bar"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap().output;
        assert!(output.contains("$HOME/foo --bar"));
    }

    #[test]
    fn test_legacy_manifest_rejected() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let tool_dir = temp.path().join(".topagent/tools/legacy_tool");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(
            tool_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "name": "legacy_tool",
                "description": "legacy tool without argv_template",
                "command": "echo",
                "args_template": "LEGACY {msg}",
                "verified": true
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(tool_dir.join("script.sh"), "echo script").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert_eq!(
            loaded.len(),
            0,
            "legacy manifest without argv_template should be rejected"
        );

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "legacy_tool");
        assert!(tools[0]
            .load_warning
            .as_deref()
            .unwrap_or_default()
            .contains("invalid manifest.json"));
    }

    #[test]
    fn test_delete_generated_tool_removes_from_set() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "to_delete",
                "will be deleted",
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

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);

        genesis.delete_generated_tool("to_delete").unwrap();

        let tools = genesis.list_generated_tools().unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn test_delete_missing_tool_fails() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis.delete_generated_tool("nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn test_delete_removes_verified_tool() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "verified_tool",
                "verified tool",
                "echo verified",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("verified".to_string()),
                }),
            )
            .unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert_eq!(loaded.len(), 1);

        genesis.delete_generated_tool("verified_tool").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_delete_and_recreate_replaces_tool() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "replaceable",
                "original",
                "echo original",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("original".to_string()),
                }),
            )
            .unwrap();

        genesis.delete_generated_tool("replaceable").unwrap();

        genesis
            .create_tool(
                "replaceable",
                "replacement",
                "echo replacement",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("replacement".to_string()),
                }),
            )
            .unwrap();

        let tools = genesis.list_generated_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].description, "replacement");
    }

    #[test]
    fn test_bad_tool_can_be_deleted() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "bad_tool",
                "broken tool",
                "exit 1",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: None,
                }),
            )
            .unwrap();

        assert!(!result.success);

        genesis.delete_generated_tool("bad_tool").unwrap();

        let tools = genesis.list_generated_tools().unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn test_structured_tool_can_be_deleted() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "echo_tool",
                "echo args tool",
                "echo \"$@\"",
                vec![ToolInput {
                    name: "msg".to_string(),
                    description: "message to echo".to_string(),
                    required: true,
                }],
                vec!["{msg}".to_string()],
                Some(VerificationSpec {
                    verification_inputs: BTreeMap::new(),
                    verification_args: vec!["test".to_string()],
                    expected_exit: 0,
                    expected_output_contains: Some("test".to_string()),
                }),
            )
            .unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert_eq!(loaded.len(), 1);

        genesis.delete_generated_tool("echo_tool").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_delete_generated_tool_via_tool() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "tool_to_delete",
                "will be deleted via tool",
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

        let tool = DeleteGeneratedToolTool::new();
        let result = tool.execute(serde_json::json!({"name": "tool_to_delete"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("deleted"));

        let tools = genesis.list_generated_tools().unwrap();
        assert!(tools.is_empty());
    }
}

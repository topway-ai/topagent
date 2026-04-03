use crate::error::Error;
use crate::external::ExternalTool;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

const TOOLS_DIR: &str = ".topagent/tools";
const GENESIS_DIR: &str = ".topagent/tool-genesis";
const PROPOSALS_DIR: &str = "proposals";

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
    pub verification: Option<VerificationSpec>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub inputs: Vec<ToolInput>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    #[default]
    Proposed,
    Approved,
    Implemented,
    Verified,
    Rejected,
}

impl std::fmt::Display for ProposalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposalStatus::Proposed => write!(f, "proposed"),
            ProposalStatus::Approved => write!(f, "approved"),
            ProposalStatus::Implemented => write!(f, "implemented"),
            ProposalStatus::Verified => write!(f, "verified"),
            ProposalStatus::Rejected => write!(f, "rejected"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolDesign {
    pub requirement: String,
    pub name: String,
    pub description: String,
    pub rationale: String,
    pub inputs: Vec<ToolInput>,
    pub argv_template: Vec<String>,
    pub verification: VerificationPlan,
    pub status: ProposalStatus,
    pub created_at: String,
    #[serde(default)]
    pub approved_at: Option<String>,
    #[serde(default)]
    pub rejected_at: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub revised_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationPlan {
    pub verification_args: Vec<String>,
    pub expected_exit: i32,
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

    pub fn genesis_dir(&self) -> PathBuf {
        self.workspace_root.join(GENESIS_DIR)
    }

    pub fn proposals_dir(&self) -> PathBuf {
        self.genesis_dir().join(PROPOSALS_DIR)
    }

    pub fn save_proposal(&self, design: &ToolDesign) -> Result<PathBuf> {
        let proposals_dir = self.proposals_dir();
        std::fs::create_dir_all(&proposals_dir)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

        let proposal_path = proposals_dir.join(format!("{}.json", design.name));
        let json =
            serde_json::to_string_pretty(design).map_err(|e| Error::InvalidInput(e.to_string()))?;
        std::fs::write(&proposal_path, json).map_err(Error::Io)?;
        Ok(proposal_path)
    }

    pub fn load_proposal(&self, name: &str) -> Result<ToolDesign> {
        let proposal_path = self.proposals_dir().join(format!("{}.json", name));
        let content = std::fs::read_to_string(&proposal_path).map_err(Error::Io)?;
        serde_json::from_str(&content).map_err(|e| Error::InvalidInput(e.to_string()))
    }

    pub fn list_proposals(&self) -> Result<Vec<ToolDesign>> {
        let proposals_dir = self.proposals_dir();
        if !proposals_dir.exists() {
            return Ok(Vec::new());
        }

        let mut proposals = Vec::new();
        for entry in std::fs::read_dir(&proposals_dir).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(proposal) = serde_json::from_str::<ToolDesign>(&content) {
                        proposals.push(proposal);
                    }
                }
            }
        }
        Ok(proposals)
    }

    pub fn update_proposal_status(&self, name: &str, status: ProposalStatus) -> Result<()> {
        let mut proposal = self.load_proposal(name)?;
        proposal.status = status;
        self.save_proposal(&proposal)?;
        Ok(())
    }

    pub fn update_proposal_metadata(
        &self,
        name: &str,
        status: ProposalStatus,
        approved_at: Option<String>,
        rejected_at: Option<String>,
        reason: Option<String>,
        revised_at: Option<String>,
    ) -> Result<()> {
        let mut proposal = self.load_proposal(name)?;
        proposal.status = status;
        proposal.approved_at = approved_at.or(proposal.approved_at);
        proposal.rejected_at = rejected_at.or(proposal.rejected_at);
        proposal.reason = reason.or(proposal.reason);
        proposal.revised_at = revised_at.or(proposal.revised_at);
        self.save_proposal(&proposal)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_tool(
        &self,
        requirement: &str,
        name: &str,
        description: &str,
        command: &str,
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

            if manifest.manifest_version.is_none() {
                eprintln!(
                    "tool '{}' has no manifest_version, skipping (regenerate with create_tool or repair_tool)",
                    manifest.name
                );
                continue;
            }

            if let Err(e) = validate_manifest_interface(&manifest) {
                eprintln!(
                    "tool '{}' has invalid interface: {}, skipping",
                    manifest.name, e
                );
                continue;
            }

            let mut full_argv = vec![script_path.display().to_string()];
            full_argv.extend(manifest.argv_template.clone());

            let input_schema = build_input_schema(&manifest.inputs);

            let tool = ExternalTool::new(&manifest.name, &manifest.description, "sh")
                .with_argv_template(full_argv)
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

    pub fn delete_generated_tool(&self, name: &str) -> Result<()> {
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

fn validate_manifest_interface(manifest: &ToolManifest) -> Result<()> {
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

mod generated_tools;
mod proposal_tools;

pub use generated_tools::{
    CreateToolTool, DeleteGeneratedToolTool, ListGeneratedToolsTool, RepairToolTool,
};
pub use proposal_tools::{
    ApproveToolProposalTool, DesignToolTool, ImplementToolProposalTool, ListToolProposalsTool,
    RejectToolProposalTool, ReviseToolProposalTool, ShowToolProposalTool,
};

fn get_string(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))
}

fn get_optional_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
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
                "echo hello",
                "echo_hello",
                "echo hello",
                "echo hello",
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
                vec![],
                vec![],
                Some(VerificationSpec {
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

        let tool_dir = temp.path().join(".topagent/tools/bad_manifest");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("manifest.json"), "not valid json{{{").unwrap();

        let loaded = genesis.load_verified_tools().unwrap();
        assert!(loaded.is_empty(), "corrupt manifest should not be loaded");
    }

    #[test]
    fn test_structured_generated_tool_reusable_with_spaces() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let result = genesis
            .create_tool(
                "echo with args",
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
                "printf special chars",
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
    }

    #[test]
    fn test_design_tool_creates_proposal() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let tool = DesignToolTool::new();

        let args = serde_json::json!({
            "requirement": "I need a tool that echoes a message",
            "name": "echo_msg",
            "description": "Echoes the provided message",
            "rationale": "Useful for testing",
            "inputs": [
                {"name": "msg", "description": "message to echo", "required": true}
            ],
            "argv_template": ["echo", "{msg}"],
            "verification_args": ["hello"],
            "expected_exit": 0,
            "expected_output_contains": "hello"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("tool design proposal saved"));
        assert!(output.contains("proposal"));
    }

    #[test]
    fn test_proposal_persisted_to_proposals_dir() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test requirement".to_string(),
            name: "test_proposal".to_string(),
            description: "A test proposal".to_string(),
            rationale: "Testing".to_string(),
            inputs: vec![ToolInput {
                name: "input".to_string(),
                description: "an input".to_string(),
                required: true,
            }],
            argv_template: vec!["echo".to_string(), "{input}".to_string()],
            verification: VerificationPlan {
                verification_args: vec!["test".to_string()],
                expected_exit: 0,
                expected_output_contains: Some("test".to_string()),
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };

        let path = genesis.save_proposal(&design).unwrap();
        assert!(path.to_string_lossy().contains("proposals"));
        assert!(path.to_string_lossy().contains("test_proposal.json"));

        let loaded = genesis.load_proposal("test_proposal").unwrap();
        assert_eq!(loaded.name, "test_proposal");
        assert_eq!(loaded.requirement, "test requirement");
    }

    #[test]
    fn test_implement_proposal_creates_tool() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "echo a message".to_string(),
            name: "echo_proposed".to_string(),
            description: "Echoes provided message".to_string(),
            rationale: "Testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            },
            status: ProposalStatus::Approved,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ImplementToolProposalTool::new();
        let args = serde_json::json!({
            "name": "echo_proposed",
            "script": "echo ok"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("created and verified"));

        let proposal = genesis.load_proposal("echo_proposed").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Verified);
    }

    #[test]
    fn test_implement_already_implemented_proposal_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "echo".to_string(),
            name: "already_done".to_string(),
            description: "already done".to_string(),
            rationale: "test".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Implemented,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ImplementToolProposalTool::new();
        let args = serde_json::json!({
            "name": "already_done",
            "script": "echo test"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("already been implemented"));
    }

    #[test]
    fn test_list_tool_proposals() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "proposal_one".to_string(),
            description: "First proposal".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ListToolProposalsTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("proposal_one"));
        assert!(output.contains("proposed"));
    }

    #[test]
    fn test_approve_tool_proposal() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "test_approve".to_string(),
            description: "Test approval".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ApproveToolProposalTool::new();
        let result = tool.execute(serde_json::json!({"name": "test_approve"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("approved"));

        let proposal = genesis.load_proposal("test_approve").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }

    #[test]
    fn test_approve_already_approved_proposal_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "already_approved".to_string(),
            description: "already approved".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Approved,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ApproveToolProposalTool::new();
        let result = tool.execute(serde_json::json!({"name": "already_approved"}), &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already approved"));
    }

    #[test]
    fn test_reject_tool_proposal() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "test_reject".to_string(),
            description: "Test rejection".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = RejectToolProposalTool::new();
        let result = tool.execute(serde_json::json!({"name": "test_reject"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("rejected"));

        let proposal = genesis.load_proposal("test_reject").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    #[test]
    fn test_reject_approved_proposal() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "reject_approved".to_string(),
            description: "Reject an approved proposal".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Approved,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = RejectToolProposalTool::new();
        let result = tool.execute(serde_json::json!({"name": "reject_approved"}), &ctx);
        assert!(result.is_ok());

        let proposal = genesis.load_proposal("reject_approved").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    #[test]
    fn test_reject_already_rejected_proposal_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "already_rejected".to_string(),
            description: "already rejected".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Rejected,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = RejectToolProposalTool::new();
        let result = tool.execute(serde_json::json!({"name": "already_rejected"}), &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already rejected"));
    }

    #[test]
    fn test_implement_unapproved_proposal_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "unapproved".to_string(),
            description: "not approved".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ImplementToolProposalTool::new();
        let result = tool.execute(
            serde_json::json!({"name": "unapproved", "script": "echo test"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not approved"));
    }

    #[test]
    fn test_implement_rejected_proposal_fails() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "test".to_string(),
            name: "rejected_proposal".to_string(),
            description: "rejected".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: None,
            },
            status: ProposalStatus::Rejected,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let tool = ImplementToolProposalTool::new();
        let result = tool.execute(
            serde_json::json!({"name": "rejected_proposal", "script": "echo test"}),
            &ctx,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("rejected"));
    }

    #[test]
    fn test_proposal_full_lifecycle() {
        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        let design = ToolDesign {
            requirement: "echo lifecycle test".to_string(),
            name: "lifecycle_test".to_string(),
            description: "Full lifecycle test".to_string(),
            rationale: "testing".to_string(),
            inputs: vec![],
            argv_template: vec![],
            verification: VerificationPlan {
                verification_args: vec![],
                expected_exit: 0,
                expected_output_contains: Some("ok".to_string()),
            },
            status: ProposalStatus::Proposed,
            created_at: "1.0".to_string(),
            ..ToolDesign::default()
        };
        genesis.save_proposal(&design).unwrap();

        let approve_tool = ApproveToolProposalTool::new();
        let result = approve_tool.execute(serde_json::json!({"name": "lifecycle_test"}), &ctx);
        assert!(result.is_ok());
        let proposal = genesis.load_proposal("lifecycle_test").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);

        let implement_tool = ImplementToolProposalTool::new();
        let result = implement_tool.execute(
            serde_json::json!({"name": "lifecycle_test", "script": "echo ok"}),
            &ctx,
        );
        assert!(result.is_ok());
        let proposal = genesis.load_proposal("lifecycle_test").unwrap();
        assert_eq!(proposal.status, ProposalStatus::Verified);
    }

    #[test]
    fn test_delete_generated_tool_removes_from_set() {
        let temp = TempDir::new().unwrap();
        let genesis = ToolGenesis::new(temp.path().to_path_buf());

        genesis
            .create_tool(
                "test",
                "to_delete",
                "will be deleted",
                "echo ok",
                vec![],
                vec![],
                Some(VerificationSpec {
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
                "test",
                "verified_tool",
                "verified tool",
                "echo verified",
                vec![],
                vec![],
                Some(VerificationSpec {
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
                "test",
                "replaceable",
                "original",
                "echo original",
                vec![],
                vec![],
                Some(VerificationSpec {
                    verification_args: vec![],
                    expected_exit: 0,
                    expected_output_contains: Some("original".to_string()),
                }),
            )
            .unwrap();

        genesis.delete_generated_tool("replaceable").unwrap();

        genesis
            .create_tool(
                "test",
                "replaceable",
                "replacement",
                "echo replacement",
                vec![],
                vec![],
                Some(VerificationSpec {
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
                "test",
                "bad_tool",
                "broken tool",
                "exit 1",
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
                "echo with args",
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
                "test",
                "tool_to_delete",
                "will be deleted via tool",
                "echo ok",
                vec![],
                vec![],
                Some(VerificationSpec {
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

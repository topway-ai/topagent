use crate::Result;
use crate::command_exec::CommandSandboxPolicy;
use crate::error::Error;
use crate::external::{ExternalTool, resolve_argv_template};
use crate::tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

const TOOLS_DIR: &str = ".topagent/tools";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub description: String,
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
    #[serde(default)]
    pub script_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationSpec {
    #[serde(default)]
    pub verification_inputs: BTreeMap<String, String>,
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
        authoring::create_tool(
            self,
            name,
            description,
            command,
            inputs,
            argv_template,
            verification,
        )
    }

    pub fn repair_tool(
        &self,
        name: &str,
        new_command: &str,
        new_inputs: Option<Vec<ToolInput>>,
        new_argv_template: Option<Vec<String>>,
        new_verification: Option<&VerificationSpec>,
    ) -> Result<GenesisResult> {
        authoring::repair_tool(
            self,
            name,
            new_command,
            new_inputs,
            new_argv_template,
            new_verification,
        )
    }

    pub fn verify_tool(&self, name: &str, spec: &VerificationSpec) -> Result<bool> {
        authoring::verify_tool(self, name, spec)
    }

    pub fn load_verified_tools(&self) -> Result<Vec<ExternalTool>> {
        Ok(self.runtime_generated_tool_inventory()?.verified_tools)
    }

    pub fn list_generated_tools(&self) -> Result<Vec<GeneratedToolSummary>> {
        Ok(self.generated_tool_inventory()?.summaries)
    }

    pub fn generated_tool_inventory(&self) -> Result<GeneratedToolInventory> {
        maintenance::generated_tool_inventory(self)
    }

    pub fn runtime_generated_tool_inventory(&self) -> Result<RuntimeGeneratedToolInventory> {
        runtime_inventory::runtime_generated_tool_inventory(self)
    }

    pub fn delete_generated_tool(&self, name: &str) -> Result<()> {
        authoring::delete_generated_tool(self, name)
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

fn script_sha256_hex(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{:02x}", byte);
    }
    encoded
}

fn script_sha256_for_path(path: &Path) -> std::io::Result<String> {
    let contents = std::fs::read(path)?;
    Ok(script_sha256_hex(&contents))
}

fn validate_verification_spec(manifest: &ToolManifest, spec: &VerificationSpec) -> Result<()> {
    let _ = verification_payload_for_manifest(manifest, spec)?;
    Ok(())
}

fn verification_payload_for_manifest(
    manifest: &ToolManifest,
    spec: &VerificationSpec,
) -> Result<serde_json::Value> {
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
    Ok(payload)
}

fn verification_command_argv(
    manifest: &ToolManifest,
    script_path: &Path,
    spec: &VerificationSpec,
) -> Result<Vec<String>> {
    let mut argv = vec![script_path.display().to_string()];
    let payload = verification_payload_for_manifest(manifest, spec)?;
    argv.extend(resolve_argv_template(
        &manifest.argv_template,
        &payload,
        &manifest.name,
    )?);
    Ok(argv)
}

fn build_input_schema(inputs: &[ToolInput]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for input in inputs {
        properties.insert(
            input.name.clone(),
            serde_json::json!({
                "type": "string",
                "description": input.description
            }),
        );
    }
    let required: Vec<_> = inputs.iter().map(|input| input.name.clone()).collect();
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
        .with_sandbox_policy(CommandSandboxPolicy::Workspace)
}

mod authoring;
mod generated_tools;
mod maintenance;
mod revalidation;
mod runtime_inventory;

#[cfg(not(test))]
fn note_runtime_inventory_scan() {}

#[cfg(not(test))]
fn note_maintenance_scan() {}

#[cfg(not(test))]
fn note_revalidation_scan() {}

#[cfg(test)]
fn note_runtime_inventory_scan() {
    test_support::note_runtime_inventory_scan();
}

#[cfg(test)]
fn note_maintenance_scan() {
    test_support::note_maintenance_scan();
}

#[cfg(test)]
fn note_revalidation_scan() {
    test_support::note_revalidation_scan();
}

pub use generated_tools::{
    CreateToolTool, DeleteGeneratedToolTool, ListGeneratedToolsTool, RepairToolTool,
};
pub use maintenance::{GeneratedToolInventory, GeneratedToolSummary};
pub(crate) use revalidation::{GeneratedToolRevalidationOutcome, revalidate_runtime_tool};
pub use runtime_inventory::{
    GeneratedToolRuntimeGuard, GeneratedToolRuntimeWarning, RuntimeGeneratedToolInventory,
};

pub fn register_generated_tool_authoring_tools(registry: &mut ToolRegistry) {
    registry.add(Box::new(CreateToolTool::new()));
    registry.add(Box::new(RepairToolTool::new()));
    registry.add(Box::new(ListGeneratedToolsTool::new()));
    registry.add(Box::new(DeleteGeneratedToolTool::new()));
}

pub fn load_generated_tool_inventory(workspace_root: &Path) -> Result<GeneratedToolInventory> {
    ToolGenesis::new(workspace_root.to_path_buf()).generated_tool_inventory()
}

pub fn load_runtime_generated_tool_inventory(
    workspace_root: &Path,
) -> Result<RuntimeGeneratedToolInventory> {
    ToolGenesis::new(workspace_root.to_path_buf()).runtime_generated_tool_inventory()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static RUNTIME_INVENTORY_SCANS: AtomicUsize = AtomicUsize::new(0);
    static MAINTENANCE_SCANS: AtomicUsize = AtomicUsize::new(0);
    static REVALIDATION_SCANS: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn note_runtime_inventory_scan() {
        RUNTIME_INVENTORY_SCANS.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_maintenance_scan() {
        MAINTENANCE_SCANS.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_revalidation_scan() {
        REVALIDATION_SCANS.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn reset_generated_tool_scan_counts() {
        RUNTIME_INVENTORY_SCANS.store(0, Ordering::Relaxed);
        MAINTENANCE_SCANS.store(0, Ordering::Relaxed);
        REVALIDATION_SCANS.store(0, Ordering::Relaxed);
    }

    pub(crate) fn generated_tool_scan_counts() -> (usize, usize) {
        (
            RUNTIME_INVENTORY_SCANS.load(Ordering::Relaxed),
            MAINTENANCE_SCANS.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn generated_tool_revalidation_count() -> usize {
        REVALIDATION_SCANS.load(Ordering::Relaxed)
    }
}

fn get_string(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::InvalidInput(format!("missing or invalid '{}' field", key)))
}

#[cfg(test)]
mod tests;

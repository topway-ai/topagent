use super::{external_tool_from_manifest, validate_manifest_interface, ToolGenesis, ToolManifest};
use crate::external::ExternalTool;
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedToolRuntimeWarning {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedToolRuntimeGuard {
    pub(crate) tool_name: String,
    pub(crate) manifest_path: std::path::PathBuf,
    pub(crate) script_path: std::path::PathBuf,
    pub(crate) expected_script_sha256: String,
}

impl GeneratedToolRuntimeGuard {
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeGeneratedToolInventory {
    pub verified_tools: Vec<ExternalTool>,
    pub runtime_guards: Vec<GeneratedToolRuntimeGuard>,
    pub warnings: Vec<GeneratedToolRuntimeWarning>,
}

impl RuntimeGeneratedToolInventory {
    pub fn warning_lines(&self, limit: usize) -> Vec<String> {
        if self.warnings.is_empty() || limit == 0 {
            return Vec::new();
        }

        let mut lines: Vec<String> = self
            .warnings
            .iter()
            .take(limit)
            .map(|warning| format!("{}: {}", warning.name, warning.message))
            .collect();
        let omitted = self.warnings.len().saturating_sub(limit);
        if omitted > 0 {
            lines.push(format!(
                "{} more generated tools are unavailable but omitted from the default warning surface.",
                omitted
            ));
        }
        lines
    }
}

pub(super) fn runtime_generated_tool_inventory(
    genesis: &ToolGenesis,
) -> Result<RuntimeGeneratedToolInventory> {
    super::note_runtime_inventory_scan();
    let tools_dir = genesis.tools_dir();
    if !tools_dir.exists() {
        return Ok(RuntimeGeneratedToolInventory::default());
    }

    let mut paths = Vec::new();
    for entry in std::fs::read_dir(tools_dir).map_err(crate::Error::Io)? {
        let entry = entry.map_err(crate::Error::Io)?;
        let path = entry.path();
        if path.is_dir() {
            paths.push(path);
        }
    }
    paths.sort();

    let mut inventory = RuntimeGeneratedToolInventory::default();
    for path in paths {
        let Some(entry) = scan_runtime_generated_tool(&path) else {
            continue;
        };
        match entry {
            RuntimeScanEntry::Callable { tool, guard } => {
                inventory.verified_tools.push(tool);
                inventory.runtime_guards.push(guard);
            }
            RuntimeScanEntry::Warning(warning) => inventory.warnings.push(warning),
        }
    }

    Ok(inventory)
}

enum RuntimeScanEntry {
    Callable {
        tool: crate::external::ExternalTool,
        guard: GeneratedToolRuntimeGuard,
    },
    Warning(GeneratedToolRuntimeWarning),
}

fn scan_runtime_generated_tool(path: &std::path::Path) -> Option<RuntimeScanEntry> {
    let manifest_path = path.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest: ToolManifest = serde_json::from_str(&content).ok()?;
    if !manifest.verified {
        return None;
    }

    let warning = if manifest.manifest_version.is_none() {
        Some(
            "missing manifest_version; repair or recreate the tool to make it callable again"
                .to_string(),
        )
    } else if let Err(err) = validate_manifest_interface(&manifest) {
        Some(format!("invalid interface: {}", err))
    } else {
        let script_path = path.join("script.sh");
        if !script_path.exists() {
            Some("missing script.sh".to_string())
        } else if manifest.script_sha256.as_deref().unwrap_or("").is_empty() {
            Some("missing script_sha256; repair or recreate the tool".to_string())
        } else {
            None
        }
    };

    if let Some(message) = warning {
        return Some(RuntimeScanEntry::Warning(GeneratedToolRuntimeWarning {
            name: manifest.name,
            message,
        }));
    }

    let script_path = path.join("script.sh");
    Some(RuntimeScanEntry::Callable {
        tool: external_tool_from_manifest(&manifest, &script_path),
        guard: GeneratedToolRuntimeGuard {
            tool_name: manifest.name,
            manifest_path,
            script_path,
            expected_script_sha256: manifest.script_sha256.unwrap_or_default(),
        },
    })
}

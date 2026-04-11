use super::{
    external_tool_from_manifest, script_sha256_for_path, validate_manifest_interface,
    GeneratedToolInventory, GeneratedToolSummary, ScannedGeneratedTool, ToolGenesis, ToolManifest,
};
use crate::Result;
use std::path::Path;

pub(super) fn generated_tool_inventory(genesis: &ToolGenesis) -> Result<GeneratedToolInventory> {
    super::note_maintenance_scan();
    let scanned = scan_generated_tools(genesis)?;
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

fn scan_generated_tools(genesis: &ToolGenesis) -> Result<Vec<ScannedGeneratedTool>> {
    let tools_dir = genesis.tools_dir();
    if !tools_dir.exists() {
        return Ok(Vec::new());
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

    Ok(paths.iter().map(|path| scan_generated_tool(path)).collect())
}

fn scan_generated_tool(path: &Path) -> ScannedGeneratedTool {
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
        Some("missing manifest_version; recreate or repair the tool to make it usable".to_string())
    } else if let Err(err) = validate_manifest_interface(&manifest) {
        Some(format!("invalid interface: {}", err))
    } else if !script_path.exists() {
        Some("missing script.sh".to_string())
    } else if manifest.verified {
        match manifest.script_sha256.as_deref() {
            None | Some("") => Some(
                "missing script_sha256; repair or recreate the tool to make it usable".to_string(),
            ),
            Some(expected_hash) => match script_sha256_for_path(&script_path) {
                Ok(current_hash) if current_hash == expected_hash => None,
                Ok(_) => Some(
                    "script.sh changed after verification; repair or recreate the tool".to_string(),
                ),
                Err(err) => Some(format!("failed to hash script.sh: {}", err)),
            },
        }
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

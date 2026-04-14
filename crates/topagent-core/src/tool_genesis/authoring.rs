use super::{
    script_sha256_hex, validate_manifest_interface, validate_tool_name, validate_verification_spec,
    verification_command_argv, GenesisResult, ToolGenesis, ToolInput, ToolManifest,
    VerificationSpec,
};
use crate::command_exec::{run_command, CommandSandboxPolicy};
use crate::error::Error;
use crate::Result;

#[allow(clippy::too_many_arguments)]
pub(super) fn create_tool(
    genesis: &ToolGenesis,
    name: &str,
    description: &str,
    command: &str,
    inputs: Vec<ToolInput>,
    argv_template: Vec<String>,
    verification: Option<VerificationSpec>,
) -> Result<GenesisResult> {
    validate_tool_name(name)?;
    let tool_dir = genesis.tools_dir().join(name);
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
        script_sha256: Some(script_sha256_hex(command.as_bytes())),
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

    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|e| Error::InvalidInput(e.to_string()))?;
    std::fs::write(&manifest_path, &manifest_json).map_err(Error::Io)?;

    let verify_result = if let Some(v) = &verification {
        let verified = verify_tool(genesis, name, v)?;
        if verified {
            let mut m = manifest;
            m.verified = true;
            let updated =
                serde_json::to_string_pretty(&m).map_err(|e| Error::InvalidInput(e.to_string()))?;
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

pub(super) fn repair_tool(
    genesis: &ToolGenesis,
    name: &str,
    new_command: &str,
    new_inputs: Option<Vec<ToolInput>>,
    new_argv_template: Option<Vec<String>>,
    new_verification: Option<&VerificationSpec>,
) -> Result<GenesisResult> {
    validate_tool_name(name)?;
    let tool_dir = genesis.tools_dir().join(name);
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
    manifest.script_sha256 = Some(script_sha256_hex(new_command.as_bytes()));
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

    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|e| Error::InvalidInput(e.to_string()))?;
    std::fs::write(&manifest_path, &manifest_json).map_err(Error::Io)?;

    let verify_result = if let Some(v) = manifest.verification.clone() {
        verify_tool(genesis, name, &v)?
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

pub(super) fn verify_tool(
    genesis: &ToolGenesis,
    name: &str,
    spec: &VerificationSpec,
) -> Result<bool> {
    validate_tool_name(name)?;
    let tool_dir = genesis.tools_dir().join(name);
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

    let output = run_command(
        "sh",
        &verification_argv,
        &genesis.workspace_root,
        None,
        CommandSandboxPolicy::Workspace,
        "generated tool verification",
    )?;

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

pub(super) fn delete_generated_tool(genesis: &ToolGenesis, name: &str) -> Result<()> {
    validate_tool_name(name)?;
    let tool_dir = genesis.tools_dir().join(name);
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

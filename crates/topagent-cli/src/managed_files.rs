use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::commands::surface::PRODUCT_NAME;
use crate::config::defaults::TOPAGENT_SERVICE_MANAGED_KEY;

pub(crate) const TOPAGENT_MANAGED_HEADER: &str =
    "# Managed by TopAgent. Safe to remove with `topagent uninstall`.";
// Note: TOPAGENT_MANAGED_HEADER is intentionally static — it is written into
// files on disk and changing it would break is_topagent_managed_file() for
// existing deployments.

pub(crate) fn read_managed_env_metadata(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_string(), parse_env_value(raw_value.trim()));
    }
    Ok(values)
}

fn parse_env_value(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut unescaped = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    unescaped.push(next);
                }
            } else {
                unescaped.push(ch);
            }
        }
        return unescaped;
    }
    value.to_string()
}

pub(crate) fn assert_managed_or_absent(path: &Path, label: &str) -> Result<()> {
    if !path.exists() || is_topagent_managed_file(path)? {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "Refusing to overwrite existing {} at {} because it was not created by {PRODUCT_NAME}. Move it aside or remove it, then run `topagent install` again.",
        label,
        path.display()
    ))
}

pub(crate) fn ensure_service_install_present(unit_path: &Path, env_path: &Path) -> Result<()> {
    if unit_path.exists() || env_path.exists() {
        return Ok(());
    }

    Err(anyhow::anyhow!(format!(
        "{PRODUCT_NAME} is not installed yet. Run `topagent install` first."
    )))
}

pub(crate) fn is_topagent_managed_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(contents
        .lines()
        .any(|line| line.trim() == TOPAGENT_MANAGED_HEADER)
        || contents.contains(&format!("{TOPAGENT_SERVICE_MANAGED_KEY}=1")))
}

pub(crate) fn write_managed_file(path: &Path, contents: &str, private: bool) -> Result<()> {
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if private { 0o600 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn remove_managed_file(path: &Path, label: &str) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    if !is_topagent_managed_file(path)? {
        return Ok(None);
    }

    std::fs::remove_file(path)
        .with_context(|| format!("failed to remove {} {}", label, path.display()))?;
    Ok(Some(format!("{} {}", label, path.display())))
}

pub(crate) fn remove_managed_env_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let env_values = read_managed_env_metadata(path)?;
    if env_values
        .get(TOPAGENT_SERVICE_MANAGED_KEY)
        .map(String::as_str)
        != Some("1")
    {
        return Ok(None);
    }

    std::fs::remove_file(path)
        .with_context(|| format!("failed to remove env file {}", path.display()))?;
    Ok(Some(format!("env file {}", path.display())))
}

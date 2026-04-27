use anyhow::{Context, Result};
use std::path::PathBuf;
use topagent_core::{
    AccessConfigDocument, CapabilityAuditLog, CapabilityGrant, CapabilityKind, CapabilityManager,
    CapabilityProfile,
};

use crate::commands::types::AccessCommands;
use crate::operational_paths::{access_audit_path, access_config_path};

pub(crate) fn load_access_manager(actor: &str, source: &str) -> Result<CapabilityManager> {
    let path = access_config_path()?;
    let document = AccessConfigDocument::load_or_default(&path)
        .with_context(|| format!("failed to read access config {}", path.display()))?;
    let audit_log = CapabilityAuditLog::new(access_audit_path()?);
    Ok(
        CapabilityManager::new(document.access, document.grants, actor, source)
            .with_store_path(path)
            .with_audit_log(audit_log),
    )
}

pub(crate) fn run_access_command(command: Option<AccessCommands>) -> Result<()> {
    let command = command.unwrap_or(AccessCommands::Status);
    let manager = load_access_manager("operator", "cli")?;
    match command {
        AccessCommands::Status => {
            print!("{}", render_access_status(&manager));
        }
        AccessCommands::Set { profile } => {
            if profile == CapabilityProfile::Full {
                println!(
                    "WARNING: full access enables broad local filesystem, shell, and network access. High-impact actions still require explicit approval."
                );
            }
            manager.set_profile(profile, format!("set from CLI to {profile}"));
            println!("Access profile set to {profile}.");
        }
        AccessCommands::Grant {
            target,
            mode,
            scope,
        } => {
            let grant = CapabilityGrant::new(
                infer_kind(&target),
                normalize_target(&target),
                mode,
                scope,
                "operator-created CLI grant",
            )
            .persisted(true);
            let id = grant.id.clone();
            manager.add_grant(grant);
            println!("Created {scope} grant {id} for {mode} access to {target}.");
        }
        AccessCommands::Revoke { target } => {
            let removed = manager.revoke_grants_for_target(&normalize_target(&target));
            if removed == 0 {
                println!("No grants matched {target}.");
            } else {
                println!("Revoked {removed} grant(s) matching {target}.");
            }
        }
        AccessCommands::Audit => {
            let audit = CapabilityAuditLog::new(access_audit_path()?);
            let records = audit.read_recent(50)?;
            if records.is_empty() {
                println!("No access audit records.");
            } else {
                for record in records {
                    println!(
                        "{} {:?} actor={} source={} profile={} decision={} kind={} target={} reason={}",
                        record.timestamp_unix,
                        record.event,
                        record.actor,
                        record.source,
                        record.profile,
                        record.decision,
                        record.kind.map(|kind| kind.to_string()).unwrap_or_else(|| "-".to_string()),
                        record.target.unwrap_or_else(|| "-".to_string()),
                        record.reason
                    );
                }
            }
        }
        AccessCommands::Lockdown => {
            manager.lockdown();
            println!("Lockdown activated: profile is workspace, network/computer_use are disabled, and grants were cleared.");
        }
    }
    Ok(())
}

pub(crate) fn render_access_status(manager: &CapabilityManager) -> String {
    let config = manager.config();
    let grants = manager.grants();
    let mut out = String::new();
    out.push_str("TopAgent access status\n");
    out.push_str(&format!("Profile: {}\n", config.profile));
    out.push_str(&format!("Network default: {}\n", config.network_default));
    out.push_str(&format!(
        "Web search default: {}\n",
        config.web_search_default
    ));
    out.push_str(&format!(
        "Computer use default: {}\n",
        config.computer_use_default
    ));
    out.push_str(&format!(
        "Workspace write: {}\n",
        config.allow_workspace_write
    ));
    out.push_str(&format!("Home read: {}\n", config.allow_home_read));
    out.push_str(&format!("Home write: {}\n", config.allow_home_write));

    let temporary = grants
        .iter()
        .filter(|grant| grant.is_temporary())
        .collect::<Vec<_>>();
    let permanent = grants
        .iter()
        .filter(|grant| !grant.is_temporary())
        .collect::<Vec<_>>();
    out.push_str("\nTemporary grants:\n");
    render_grants(&mut out, &temporary);
    out.push_str("\nPermanent grants:\n");
    render_grants(&mut out, &permanent);
    out
}

fn render_grants(out: &mut String, grants: &[&CapabilityGrant]) {
    if grants.is_empty() {
        out.push_str("- none\n");
        return;
    }
    for grant in grants {
        out.push_str(&format!(
            "- {} {} {} {} ({})\n",
            grant.id, grant.kind, grant.mode, grant.target, grant.scope
        ));
    }
}

pub(crate) fn infer_kind(target: &str) -> CapabilityKind {
    let lower = target.trim().to_ascii_lowercase();
    if lower == "network" || lower.starts_with("http://") || lower.starts_with("https://") {
        CapabilityKind::Network
    } else if lower == "web_search" || lower == "web-search" {
        CapabilityKind::WebSearch
    } else if lower == "computer_use" || lower == "computer-use" {
        CapabilityKind::ComputerUse
    } else {
        CapabilityKind::Filesystem
    }
}

pub(crate) fn normalize_target(target: &str) -> String {
    if let Some(rest) = target.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    target.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use topagent_core::{AccessConfig, CapabilityProfile};

    #[test]
    fn test_render_access_status_includes_profile_and_options() {
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Developer),
            Vec::new(),
            "test",
            "unit",
        );
        let rendered = render_access_status(&manager);
        assert!(rendered.contains("Profile: developer"));
        assert!(rendered.contains("Network default: true"));
        assert!(rendered.contains("Temporary grants:"));
        assert!(rendered.contains("Permanent grants:"));
    }
}

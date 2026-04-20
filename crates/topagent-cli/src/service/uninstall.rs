use anyhow::Result;
use std::path::{Path, PathBuf};

use super::detect::{InstallRoot, InstallRootKind, detect_install_root_from_exe};
use super::systemd::{ensure_systemd_user_available, format_systemctl_result};
use crate::config::defaults::{TELEGRAM_SERVICE_UNIT_NAME, TOPAGENT_WORKSPACE_KEY};
use crate::managed_files::{
    is_topagent_managed_file, read_managed_env_metadata, remove_managed_env_file,
    remove_managed_file,
};
use crate::operational_paths::{resolve_config_home, service_paths};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BinaryCleanupOutcome {
    Removed(String),
    Preserved(String),
}

pub(super) fn uninstall_service_setup(remove_binary: bool, purge: bool) -> Result<()> {
    let paths = service_paths()?;
    let env_values = read_managed_env_metadata(&paths.env_path).unwrap_or_default();
    let managed_unit = paths.unit_path.exists() && is_topagent_managed_file(&paths.unit_path)?;
    let managed_env = paths.env_path.exists() && is_topagent_managed_file(&paths.env_path)?;
    let should_manage_service = managed_unit || managed_env;
    let systemd_available = ensure_systemd_user_available().map_err(|e| e.to_string());
    let mut stopped = String::from("not attempted");
    let mut disabled = String::from("not attempted");

    if should_manage_service && systemd_available.is_ok() {
        stopped = format_systemctl_result(&["stop", TELEGRAM_SERVICE_UNIT_NAME]);
        disabled = format_systemctl_result(&["disable", TELEGRAM_SERVICE_UNIT_NAME]);
    } else if should_manage_service {
        if let Err(err) = &systemd_available {
            let note = format!("not attempted ({})", err);
            stopped = note.clone();
            disabled = note;
        }
    } else if let Err(err) = &systemd_available {
        let note = format!("no managed service found ({})", err);
        stopped = note.clone();
        disabled = note;
    } else {
        stopped = "no managed service found".to_string();
        disabled = "no managed service found".to_string();
    }

    let mut removed = Vec::new();
    let mut preserved = Vec::new();

    match remove_managed_file(&paths.unit_path, "unit file")? {
        Some(path) => removed.push(path),
        None => {
            if paths.unit_path.exists() {
                preserved.push(format!(
                    "unit file left in place (not managed by TopAgent): {}",
                    paths.unit_path.display()
                ));
            }
        }
    }

    match remove_managed_env_file(&paths.env_path)? {
        Some(path) => removed.push(path),
        None => {
            if paths.env_path.exists() {
                preserved.push(format!(
                    "env file left in place (not managed by TopAgent): {}",
                    paths.env_path.display()
                ));
            }
        }
    }

    let workspace_path = env_values.get(TOPAGENT_WORKSPACE_KEY);

    if purge {
        if let Some(workspace) = workspace_path {
            let ws_path = PathBuf::from(workspace);
            let topagent_dir = ws_path.join(".topagent");
            if topagent_dir.exists() && topagent_dir.is_dir() {
                if let Err(err) = std::fs::remove_dir_all(&topagent_dir) {
                    preserved.push(format!(
                        ".topagent/ left in place (could not remove {}: {})",
                        topagent_dir.display(),
                        err
                    ));
                } else {
                    removed.push(format!("workspace .topagent/ {}", topagent_dir.display()));
                }
            }

            let cache_dir = if let Ok(config_home) = resolve_config_home() {
                config_home.join("topagent").join("cache")
            } else {
                ws_path.join(".topagent").join("cache")
            };
            if cache_dir.exists() && cache_dir.is_dir() {
                if let Err(err) = std::fs::remove_dir_all(&cache_dir) {
                    preserved.push(format!(
                        "cache/ left in place (could not remove {}: {})",
                        cache_dir.display(),
                        err
                    ));
                } else {
                    removed.push(format!("cache directory {}", cache_dir.display()));
                }
            }
        }
    } else if let Some(workspace) = workspace_path {
        preserved.push(format!("workspace directory preserved: {}", workspace));
    }

    if remove_binary {
        match cleanup_current_binary_for_uninstall() {
            BinaryCleanupOutcome::Removed(item) => removed.push(item),
            BinaryCleanupOutcome::Preserved(item) => preserved.push(item),
        }
    }

    let mut daemon_reload = String::from("not needed");
    if should_manage_service && systemd_available.is_ok() {
        daemon_reload = format_systemctl_result(&["daemon-reload"]);
    }

    println!("TopAgent uninstall");
    println!("Stopped: {}", stopped);
    println!("Disabled: {}", disabled);
    println!("Daemon reload: {}", daemon_reload);
    if purge {
        println!("Mode: purge");
    } else {
        println!("Mode: standard");
    }
    println!("Removed:");
    if removed.is_empty() {
        println!("  nothing");
    } else {
        for item in &removed {
            println!("  {}", item);
        }
    }
    println!("Left in place:");
    if preserved.is_empty() {
        println!("  nothing");
    } else {
        for item in &preserved {
            println!("  {}", item);
        }
    }

    Ok(())
}

fn cleanup_current_binary_for_uninstall() -> BinaryCleanupOutcome {
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (could not determine current binary path: {})",
                err
            ));
        }
    };

    cleanup_binary_for_uninstall_at_path(&current_exe)
}

pub(super) fn cleanup_binary_for_uninstall_at_path(exe: &Path) -> BinaryCleanupOutcome {
    let resolved_exe = match exe.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (could not resolve {}: {})",
                exe.display(),
                err
            ));
        }
    };

    match detect_install_root_from_exe(&resolved_exe) {
        Ok(InstallRoot {
            kind: InstallRootKind::SourceCheckout,
            ..
        }) => BinaryCleanupOutcome::Preserved(format!(
            "source checkout binary preserved: {}",
            resolved_exe.display()
        )),
        Ok(InstallRoot {
            kind: InstallRootKind::InstalledBinary,
            ..
        }) => match std::fs::remove_file(&resolved_exe) {
            Ok(()) => BinaryCleanupOutcome::Removed(format!(
                "installed binary {}",
                resolved_exe.display()
            )),
            Err(err) => BinaryCleanupOutcome::Preserved(format!(
                "installed binary left in place (failed to remove {}: {})",
                resolved_exe.display(),
                err
            )),
        },
        Err(err) => BinaryCleanupOutcome::Preserved(format!(
            "installed binary left in place (could not classify {}: {})",
            resolved_exe.display(),
            err
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_binary_for_uninstall_removes_installed_binary() {
        let install_dir = tempfile::TempDir::new().unwrap();
        let exe = install_dir.path().join("topagent");
        std::fs::write(&exe, "").unwrap();
        let canonical_exe = exe.canonicalize().unwrap();

        let outcome = cleanup_binary_for_uninstall_at_path(&exe);

        assert_eq!(
            outcome,
            BinaryCleanupOutcome::Removed(format!("installed binary {}", canonical_exe.display()))
        );
        assert!(!exe.exists());
    }

    #[test]
    fn test_cleanup_binary_for_uninstall_preserves_source_checkout_binary() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-cli")).unwrap();
        std::fs::create_dir_all(repo.path().join("crates").join("topagent-core")).unwrap();
        std::fs::create_dir_all(repo.path().join("target").join("debug")).unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-cli")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-cli\"\n",
        )
        .unwrap();
        std::fs::write(
            repo.path()
                .join("crates")
                .join("topagent-core")
                .join("Cargo.toml"),
            "[package]\nname = \"topagent-core\"\n",
        )
        .unwrap();
        let exe = repo.path().join("target").join("debug").join("topagent");
        std::fs::write(&exe, "").unwrap();

        let outcome = cleanup_binary_for_uninstall_at_path(&exe);

        assert_eq!(
            outcome,
            BinaryCleanupOutcome::Preserved(format!(
                "source checkout binary preserved: {}",
                exe.canonicalize().unwrap().display()
            ))
        );
        assert!(exe.exists());
    }
}

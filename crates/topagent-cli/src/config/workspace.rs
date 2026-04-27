use anyhow::Context;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_workspace_path(workspace: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    resolve_workspace_path_with_current_dir(workspace, std::env::current_dir())
}

pub(crate) fn resolve_workspace_path_with_current_dir(
    workspace: Option<PathBuf>,
    current_dir: std::io::Result<PathBuf>,
) -> anyhow::Result<PathBuf> {
    let workspace = match workspace {
        Some(path) => path,
        None => current_dir.context(
            "Failed to determine the current directory. Run TopAgent from your repo or pass --workspace /path/to/repo.",
        )?,
    };

    if !workspace.exists() {
        return Err(anyhow::anyhow!(
            "Workspace path does not exist: {}. Run TopAgent from a repo directory or pass --workspace /path/to/repo.",
            workspace.display()
        ));
    }

    if !workspace.is_dir() {
        return Err(anyhow::anyhow!(
            "Workspace path is not a directory: {}",
            workspace.display()
        ));
    }

    reject_binary_installation_path(&workspace)?;

    workspace.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Workspace path is not accessible: {} ({})",
            workspace.display(),
            e
        )
    })
}

/// Reject workspace paths that look like binary installation directories
/// rather than project directories. This prevents misconfiguration where
/// the systemd service `WorkingDirectory` would point to e.g.
/// `~/.cargo/bin/` instead of a real project.
fn reject_binary_installation_path(workspace: &Path) -> anyhow::Result<()> {
    // Check the last two path components against well-known binary
    // directory patterns. Using exact component matching (not substring)
    // avoids false positives on paths like
    //   /home/user/projects/bin-management-tool/
    // which contain "bin" but are legitimate project directories.
    let file_name = workspace.file_name().unwrap_or_default().to_string_lossy();
    let parent_name = workspace
        .parent()
        .and_then(|p| p.file_name())
        .unwrap_or_default()
        .to_string_lossy();

    let is_known_bin_dir = (file_name == "bin"
        && matches!(
            parent_name.as_ref(),
            ".cargo" | ".local" | "usr" | "local" | ""
        ))
        || (file_name == "sbin" && parent_name.is_empty());

    if is_known_bin_dir {
        return Err(anyhow::anyhow!(
            "Workspace path ({}) looks like a binary installation directory, not a project directory. \
            Set TOPAGENT_WORKSPACE to your project directory instead.",
            workspace.display()
        ));
    }

    // Also check if the workspace contains a well-known binary but no
    // project markers (no Cargo.toml, package.json, go.mod, .git, etc.)
    // and is a very flat directory with executables.
    if let Ok(entries) = std::fs::read_dir(workspace) {
        let has_project_marker = workspace.join("Cargo.toml").exists()
            || workspace.join("package.json").exists()
            || workspace.join("go.mod").exists()
            || workspace.join(".git").exists()
            || workspace.join("TOPAGENT.md").exists()
            || workspace.join(".topagent").exists();

        if !has_project_marker {
            let exe_count = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter(|e| {
                    // Check if file has executable bit set (Unix only)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        e.metadata()
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                    }
                    #[cfg(not(unix))]
                    {
                        e.path()
                            .extension()
                            .map(|ext| ext == "exe")
                            .unwrap_or(false)
                    }
                })
                .count();
            // If there are 3+ executables and no project markers, it's likely
            // a bin directory rather than a project workspace.
            if exe_count >= 3 {
                return Err(anyhow::anyhow!(
                    "Workspace path ({}) appears to be a binary directory (found {} executables, no project markers). \
                    Set TOPAGENT_WORKSPACE to your project directory instead.",
                    workspace.display(),
                    exe_count
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_workspace_defaults_to_current_directory_for_one_shot_and_telegram() {
        let temp = TempDir::new().unwrap();
        let resolved =
            resolve_workspace_path_with_current_dir(None, Ok(temp.path().to_path_buf())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_override_beats_current_directory_for_one_shot_and_telegram() {
        let current = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(override_dir.path().to_path_buf()),
            Ok(current.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_resolution_fails_when_current_directory_is_unavailable() {
        let err = resolve_workspace_path_with_current_dir(
            None,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Failed to determine the current directory"));
    }

    #[test]
    fn test_workspace_override_ignores_invalid_current_directory() {
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(PathBuf::from(override_dir.path())),
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_rejects_cargo_bin_directory() {
        let cargo_bin = TempDir::new().unwrap();
        // Create the path structure ~/.cargo/bin/ by naming the temp dir
        let fake_cargo_bin = cargo_bin.path().join(".cargo").join("bin");
        std::fs::create_dir_all(&fake_cargo_bin).unwrap();
        let err = resolve_workspace_path_with_current_dir(
            Some(fake_cargo_bin.clone()),
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("binary installation directory"),
            "expected binary-dir rejection, got: {err}"
        );
    }

    #[test]
    fn test_workspace_rejects_flat_exe_dir_without_project_markers() {
        let bin_dir = TempDir::new().unwrap();
        // Create 3 executable files with no project markers
        for name in ["tool-a", "tool-b", "tool-c"] {
            let path = bin_dir.path().join(name);
            std::fs::write(&path, b"#!/bin/sh\necho hi").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let err = resolve_workspace_path_with_current_dir(
            Some(bin_dir.path().to_path_buf()),
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("binary directory"),
            "expected binary-dir rejection for flat exe dir, got: {err}"
        );
    }

    #[test]
    fn test_workspace_accepts_dir_with_cargo_toml_even_if_exes_present() {
        let project_dir = TempDir::new().unwrap();
        // Create an executable file
        let exe_path = project_dir.path().join("build-script");
        std::fs::write(&exe_path, b"#!/bin/sh\necho build").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Also create a Cargo.toml (project marker) — this should override
        // the executable heuristic.
        std::fs::write(
            project_dir.path().join("Cargo.toml"),
            "[package]\nname=\"test\"\n",
        )
        .unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(project_dir.path().to_path_buf()),
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        )
        .unwrap();
        assert_eq!(resolved, project_dir.path().canonicalize().unwrap());
    }
}

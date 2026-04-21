use crate::commands::surface::PRODUCT_NAME;
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallRootKind {
    SourceCheckout,
    InstalledBinary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallRoot {
    pub(super) kind: InstallRootKind,
    pub(super) root: PathBuf,
}

pub(super) fn resolve_current_exe() -> Result<PathBuf> {
    std::env::current_exe()
        .context(format!("cannot determine the {PRODUCT_NAME} binary path"))?
        .canonicalize()
        .context(format!("cannot resolve the {PRODUCT_NAME} binary path"))
}

pub(super) fn detect_install_root() -> Result<PathBuf> {
    Ok(detect_install_root_from_exe(&resolve_current_exe()?)?.root)
}

pub(super) fn detect_install_root_from_exe(exe: &Path) -> Result<InstallRoot> {
    if let Some(target_dir) = exe
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "target"))
    {
        let repo_root = target_dir.parent().ok_or_else(|| {
            anyhow!(
                "{PRODUCT_NAME} is running from a target directory, but the repo root could not be determined."
            )
        })?;
        if looks_like_source_checkout(repo_root) {
            return Ok(InstallRoot {
                kind: InstallRootKind::SourceCheckout,
                root: repo_root.to_path_buf(),
            });
        }
        return Err(anyhow!(
            "{PRODUCT_NAME} is running from a target directory, but this does not look like a {PRODUCT_NAME} source checkout. Re-run from the repo root or install the binary into a stable directory before `topagent install`."
        ));
    }

    let install_dir = exe.parent().ok_or_else(|| {
        anyhow!("Could not determine the directory that contains the {PRODUCT_NAME} binary.")
    })?;
    Ok(InstallRoot {
        kind: InstallRootKind::InstalledBinary,
        root: install_dir.to_path_buf(),
    })
}

fn looks_like_source_checkout(root: &Path) -> bool {
    root.join("Cargo.toml").is_file()
        && root
            .join("crates")
            .join("topagent-cli")
            .join("Cargo.toml")
            .is_file()
        && root
            .join("crates")
            .join("topagent-core")
            .join("Cargo.toml")
            .is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_install_root_uses_repo_root_for_source_checkout_binary() {
        let repo = TempDir::new().unwrap();
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

        let detected = detect_install_root_from_exe(&exe).unwrap();

        assert_eq!(detected.kind, InstallRootKind::SourceCheckout);
        assert_eq!(detected.root, repo.path());
    }

    #[test]
    fn test_detect_install_root_uses_binary_directory_for_installed_binary() {
        let install_dir = TempDir::new().unwrap();
        let exe = install_dir.path().join("topagent");
        std::fs::write(&exe, "").unwrap();

        let detected = detect_install_root_from_exe(&exe).unwrap();

        assert_eq!(detected.kind, InstallRootKind::InstalledBinary);
        assert_eq!(detected.root, install_dir.path());
    }

    #[test]
    fn test_detect_install_root_rejects_ambiguous_target_layout() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("target").join("debug")).unwrap();
        let exe = temp.path().join("target").join("debug").join("topagent");
        std::fs::write(&exe, "").unwrap();

        let err = detect_install_root_from_exe(&exe).unwrap_err().to_string();

        assert!(err.contains("does not look like a TopAgent source checkout"));
    }
}

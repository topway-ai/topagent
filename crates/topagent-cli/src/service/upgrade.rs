use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::commands::surface::PRODUCT_NAME;
use crate::config::defaults::TELEGRAM_SERVICE_UNIT_NAME;

const RELEASE_BASE_URL: &str = "https://github.com/topway-ai/topagent/releases/latest/download";

/// Supported release target for the current host. Returns `None` on unsupported
/// platforms so the caller can fall back to `--use-cargo`.
pub(crate) fn release_target() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some("x86_64-unknown-linux-gnu");

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    return None;
}

/// Replace the running binary with the latest GitHub release, stopping and
/// restarting the systemd service around the swap when it is active.
pub(crate) fn run_upgrade(use_cargo: bool) -> Result<()> {
    let target_bin = resolve_upgrade_target()?;
    println!("{PRODUCT_NAME} upgrade");
    println!("Binary: {}", target_bin.display());

    if use_cargo {
        upgrade_from_cargo(&target_bin)
    } else {
        let Some(target) = release_target() else {
            bail!(
                "No precompiled release is published for {} {}. \
                 Re-run with --use-cargo to build from source.",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
        };
        upgrade_from_release(&target_bin, target)
    }
}

fn upgrade_from_release(target_bin: &Path, target: &str) -> Result<()> {
    let asset_name = format!("topagent-{target}");
    let asset_url = format!("{RELEASE_BASE_URL}/{asset_name}");
    let checksum_url = format!("{RELEASE_BASE_URL}/{asset_name}.sha256");

    println!("Downloading latest release for {target}...");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client")?;

    let binary_bytes = fetch_bytes(&client, &asset_url)
        .with_context(|| format!("failed to download {asset_url}"))?;

    println!("Verifying checksum...");
    let checksum_text = fetch_text(&client, &checksum_url)
        .with_context(|| format!("failed to download checksum from {checksum_url}"))?;

    verify_sha256(&binary_bytes, &checksum_text, &asset_name)?;

    let was_active = service_is_active();
    if was_active {
        println!("Stopping service...");
        stop_service();
    }

    atomic_replace(target_bin, &binary_bytes)
        .with_context(|| format!("failed to replace {}", target_bin.display()))?;

    println!("Binary replaced.");

    if was_active {
        println!("Restarting service...");
        restart_service();
    }

    print_next_steps(target_bin, was_active);
    Ok(())
}

fn upgrade_from_cargo(target_bin: &Path) -> Result<()> {
    println!("Building from source via cargo install...");
    println!("(This may take several minutes)");

    let was_active = service_is_active();
    if was_active {
        println!("Stopping service...");
        stop_service();
    }

    let install_root = target_bin
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());

    let mut cmd = Command::new("cargo");
    cmd.args([
        "install",
        "--locked",
        "--force",
        "--git",
        "https://github.com/topway-ai/topagent",
        "--branch",
        "main",
        "topagent-cli",
    ]);
    if let Some(root) = install_root {
        cmd.arg("--root").arg(root);
    }

    let status = cmd.status().context("failed to run `cargo install`")?;
    if !status.success() {
        bail!("cargo install failed (exit status {})", status);
    }

    if was_active {
        println!("Restarting service...");
        restart_service();
    }

    print_next_steps(target_bin, was_active);
    Ok(())
}

fn fetch_bytes(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("request failed for {url}"))?;
    if !response.status().is_success() {
        bail!("HTTP {} for {url}", response.status());
    }
    response
        .bytes()
        .map(|b| b.to_vec())
        .context("failed to read response body")
}

fn fetch_text(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
    let bytes = fetch_bytes(client, url)?;
    String::from_utf8(bytes).context("checksum file is not valid UTF-8")
}

/// Verify that the SHA256 of `data` matches the expected digest from a
/// standard `sha256sum`-format checksum file (one line: `<hex>  <filename>`).
fn verify_sha256(data: &[u8], checksum_text: &str, asset_name: &str) -> Result<()> {
    let expected = parse_sha256_checksum(checksum_text, asset_name)?;
    let actual = hex::encode(Sha256::digest(data));
    if actual != expected {
        bail!("SHA256 mismatch — expected {expected}, got {actual}");
    }
    Ok(())
}

/// Parse the expected hex digest from a `sha256sum` output line.
/// Format: `<hex-digest>  <filename>` (two spaces) or `<hex-digest> <filename>`.
pub(crate) fn parse_sha256_checksum(text: &str, asset_name: &str) -> Result<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (digest, name) = line
            .split_once(|c: char| c.is_whitespace())
            .with_context(|| format!("malformed checksum line: {line:?}"))?;
        let name = name.trim();
        // Accept bare filename or path ending in the asset name.
        let name_tail = name.rsplit('/').next().unwrap_or(name);
        if name_tail == asset_name {
            return Ok(digest.to_ascii_lowercase());
        }
    }
    bail!("no checksum entry found for {asset_name} in:\n{text}")
}

/// Atomically replace `target` with `data` using a sibling temp file + rename.
/// Sets the executable bit before replacing.
fn atomic_replace(target: &Path, data: &[u8]) -> Result<()> {
    let parent = target
        .parent()
        .with_context(|| format!("no parent dir for {}", target.display()))?;
    let tmp_path = parent.join(".topagent-upgrade.tmp");

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to open temp file {}", tmp_path.display()))?;
        f.write_all(data)
            .with_context(|| format!("failed to write temp file {}", tmp_path.display()))?;
    }

    set_executable(&tmp_path)?;
    std::fs::rename(&tmp_path, target).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            target.display()
        )
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .context("failed to read temp file metadata")?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).context("failed to set executable bit")
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Resolve the path of the binary to replace. Prefers the installed binary at
/// `~/.cargo/bin/topagent`; falls back to the current exe.
fn resolve_upgrade_target() -> Result<PathBuf> {
    // Prefer ~/.cargo/bin/topagent — this is where `cargo install` places the binary
    // and what most users have in PATH.
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin/topagent");
        if cargo_bin.exists() {
            return Ok(cargo_bin);
        }
    }
    // Fallback: replace the binary that is currently running.
    std::env::current_exe()
        .context(format!("cannot determine the {PRODUCT_NAME} binary path"))?
        .canonicalize()
        .context(format!("cannot resolve the {PRODUCT_NAME} binary path"))
}

fn service_is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", TELEGRAM_SERVICE_UNIT_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn stop_service() {
    let _ = Command::new("systemctl")
        .args(["--user", "stop", TELEGRAM_SERVICE_UNIT_NAME])
        .status();
}

fn restart_service() {
    let _ = Command::new("systemctl")
        .args(["--user", "restart", TELEGRAM_SERVICE_UNIT_NAME])
        .status();
}

fn print_next_steps(target_bin: &Path, service_was_active: bool) {
    println!();
    println!("{PRODUCT_NAME} upgraded.");
    println!("Binary: {}", target_bin.display());
    if service_was_active {
        println!("Service restarted.");
    }
    println!();
    println!("Check status:");
    println!("  topagent status");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sha256_two_space_format() {
        let text = "abc123def456  topagent-x86_64-unknown-linux-gnu\n";
        let digest = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(digest, "abc123def456");
    }

    #[test]
    fn test_parse_sha256_single_space_format() {
        let text = "deadbeef topagent-x86_64-unknown-linux-gnu\n";
        let digest = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(digest, "deadbeef");
    }

    #[test]
    fn test_parse_sha256_with_path_prefix() {
        let text = "aabbccdd  ./dist/topagent-x86_64-unknown-linux-gnu\n";
        let digest = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(digest, "aabbccdd");
    }

    #[test]
    fn test_parse_sha256_normalises_uppercase() {
        let text = "AABB1122  topagent-x86_64-unknown-linux-gnu\n";
        let digest = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(digest, "aabb1122");
    }

    #[test]
    fn test_parse_sha256_missing_asset_returns_error() {
        let text = "aabbccdd  topagent-other-target\n";
        let err = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap_err();
        assert!(err.to_string().contains("no checksum entry found"), "{err}");
    }

    #[test]
    fn test_parse_sha256_empty_lines_skipped() {
        let text = "\n\naabb1234  topagent-x86_64-unknown-linux-gnu\n\n";
        let digest = parse_sha256_checksum(text, "topagent-x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(digest, "aabb1234");
    }

    #[test]
    fn test_release_target_on_linux_x86_64() {
        // This test only asserts the target is Some on the CI platform.
        // On other platforms it may be None — that is correct behaviour.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(release_target(), Some("x86_64-unknown-linux-gnu"));
    }
}

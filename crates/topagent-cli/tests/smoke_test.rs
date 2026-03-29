use assert_cmd::Command;
use std::path::PathBuf;

#[test]
fn test_cli_smoke_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help").assert().success();
}

#[test]
fn test_cli_help_mentions_workspace() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("--workspace"));
}

#[test]
fn test_cli_help_mentions_service_command() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("service"));
}

#[test]
fn test_cli_bare_instruction_requires_api_key() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.env_remove("OPENROUTER_API_KEY")
        .args(["say hello"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY",
        ));
}

#[test]
fn test_cli_invalid_workspace_fails_fast() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args([
        "--api-key",
        "test-key",
        "--workspace",
        "/definitely/missing/path",
        "say hello",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "Workspace path does not exist: /definitely/missing/path",
    ));
}

#[test]
fn test_cli_telegram_requires_token() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.env_remove("TELEGRAM_BOT_TOKEN")
        .args(["telegram"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN",
        ));
}

#[test]
fn test_cli_telegram_invalid_token_format() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["telegram", "--token", "badtoken"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Telegram bot token looks invalid",
        ));
}

#[test]
fn test_cli_telegram_invalid_workspace_fails_fast() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.env("OPENROUTER_API_KEY", "test-key")
        .args([
            "telegram",
            "--token",
            "123456:abcdef",
            "--workspace",
            "/definitely/missing/path",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Workspace path does not exist: /definitely/missing/path",
        ));
}

#[test]
fn test_install_script_has_valid_syntax() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = repo_root.join("scripts/install.sh");

    let mut cmd = Command::new("bash");
    cmd.arg("-n").arg(script).assert().success();
}

#[test]
fn test_readme_uses_cd_into_repo_instead_of_workspace_env() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("cd /path/to/your/repo"));
    assert!(!readme.contains("TOPAGENT_WORKSPACE"));
}

#[test]
fn test_cli_uninstall_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("uninstall"));
}

#[test]
fn test_cli_uninstall_removes_copied_binary() {
    // Copy the built binary to a temp location and run uninstall on it.
    let bin = Command::cargo_bin("topagent")
        .unwrap()
        .get_program()
        .to_owned();
    let temp = tempfile::NamedTempFile::new().unwrap();
    let temp_path = temp.path().to_owned();
    // Close so we can overwrite
    drop(temp);
    std::fs::copy(&bin, &temp_path).unwrap();

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = std::process::Command::new(&temp_path)
        .arg("uninstall")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "uninstall should succeed: {}",
        stdout
    );
    assert!(
        stdout.contains("Removed"),
        "should report removal: {}",
        stdout
    );
    assert!(!temp_path.exists(), "binary should be deleted");
}

#[test]
fn test_cli_uninstall_honest_about_what_is_not_removed() {
    // Run uninstall on a temp copy and verify messaging.
    let bin = Command::cargo_bin("topagent")
        .unwrap()
        .get_program()
        .to_owned();
    let temp = tempfile::NamedTempFile::new().unwrap();
    let temp_path = temp.path().to_owned();
    drop(temp);
    std::fs::copy(&bin, &temp_path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = std::process::Command::new(&temp_path)
        .arg("uninstall")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Not removed"),
        "should list what was not removed"
    );
    assert!(
        stdout.contains("Source repo"),
        "should mention source repos"
    );
}

#[test]
fn test_readme_documents_uninstall() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent uninstall"));
}

#[test]
fn test_readme_documents_service_commands() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent service install"));
    assert!(readme.contains("topagent service status"));
    assert!(readme.contains("topagent service uninstall"));
}

#[test]
fn test_install_script_output_no_longer_teaches_workspace_env() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();

    assert!(script.contains("cd /path/to/your/repo"));
    assert!(!script.contains("TOPAGENT_WORKSPACE"));
}

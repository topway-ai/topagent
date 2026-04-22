use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

fn isolated_topagent_command() -> (TempDir, Command) {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let config = temp.path().join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&config).unwrap();

    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.env("HOME", &home).env("XDG_CONFIG_HOME", &config);
    (temp, cmd)
}

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
fn test_cli_bare_instruction_requires_api_key() {
    let (_temp, mut cmd) = isolated_topagent_command();
    cmd.env_remove("OPENROUTER_API_KEY")
        .args(["say hello"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("OpenRouter API key required").and(
                predicates::str::contains("set --api-key or OPENROUTER_API_KEY"),
            ),
        );
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
    let (_temp, mut cmd) = isolated_topagent_command();
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
fn test_cli_telegram_fails_fast_when_openrouter_api_key_missing() {
    let (_temp, mut cmd) = isolated_topagent_command();
    cmd.env_remove("OPENROUTER_API_KEY")
        .args(["telegram", "--token", "123456:abcdef"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("OpenRouter API key required"));
}

#[test]
fn test_cli_config_inspect_shows_expected_fields() {
    let (_temp, mut cmd) = isolated_topagent_command();
    cmd.args(["config", "inspect"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Provider:"))
        .stdout(predicates::str::contains("Model:"))
        .stdout(predicates::str::contains("OpenRouter:"))
        .stdout(predicates::str::contains("Opencode:"))
        .stdout(predicates::str::contains("Bot token:"))
        .stdout(predicates::str::contains("DM access:"))
        .stdout(predicates::str::contains("Timeout:"));
}

#[test]
fn test_cli_config_inspect_does_not_reveal_api_key_value() {
    let (_temp, mut cmd) = isolated_topagent_command();
    cmd.env("OPENROUTER_API_KEY", "sk-super-secret-key-value")
        .args(["config", "inspect"])
        .assert()
        .success()
        .stdout(predicates::str::contains("present"))
        .stdout(predicates::str::contains("OpenRouter:"))
        // The actual secret value must never appear in the output
        .stdout(
            predicates::str::is_match("sk-super-secret-key-value")
                .unwrap()
                .not(),
        );
}

#[test]
fn test_cli_config_inspect_graceful_without_install() {
    // In a clean isolated environment (no persisted service config), inspect
    // should still succeed and show missing for keys and token.
    let (_temp, mut cmd) = isolated_topagent_command();
    cmd.env_remove("OPENROUTER_API_KEY")
        .env_remove("OPENCODE_API_KEY")
        .env_remove("TELEGRAM_BOT_TOKEN")
        .args(["config", "inspect"])
        .assert()
        .success()
        .stdout(predicates::str::contains("missing"));
}

#[test]
fn test_cli_install_help_documents_operator_flags() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--api-key"))
        .stdout(predicates::str::contains("--opencode-api-key"))
        .stdout(predicates::str::contains("--workspace"))
        .stdout(predicates::str::contains("--model"));
}

#[test]
fn test_cli_telegram_help_documents_operator_flags() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["telegram", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--token"))
        .stdout(predicates::str::contains("--api-key"))
        .stdout(predicates::str::contains("--opencode-api-key"))
        .stdout(predicates::str::contains("--workspace"))
        .stdout(predicates::str::contains("--model"));
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
fn test_readme_documents_running_from_workspace_directory() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("cd /path/to/your/repo"));
}

#[test]
fn test_cli_status_reports_lifecycle_sources_of_truth() {
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("status")
        .assert()
        .success()
        .stdout(predicates::str::contains("Lifecycle sources of truth:"))
        .stdout(predicates::str::contains("topagent config inspect"))
        .stdout(predicates::str::contains("topagent status"))
        .stdout(predicates::str::contains("topagent run status"))
        .stdout(predicates::str::contains("topagent memory status"));
}

#[test]
fn test_cli_memory_lint_clean_workspace_ok() {
    let temp = TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/notes")).unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
    std::fs::write(
        temp.path().join(".topagent/MEMORY.md"),
        "# TopAgent Memory Index\n\n- title: arch | file: notes/arch.md | status: verified | note: layout\n",
    )
    .unwrap();
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("--workspace")
        .arg(temp.path())
        .args(["memory", "lint"])
        .assert()
        .success()
        .stdout(predicates::str::contains("OK"));
}

#[test]
fn test_cli_memory_status_reports_learning_layers_for_fresh_workspace() {
    let temp = TempDir::new().unwrap();
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("--workspace")
        .arg(temp.path())
        .args(["memory", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Operator model: 0 preference(s)"))
        .stdout(predicates::str::contains(
            "Procedures: 0 active, 0 superseded, 0 disabled",
        ))
        .stdout(predicates::str::contains(
            "Trajectories: 0 local, 0 ready, 0 exported",
        ))
        .stdout(predicates::str::contains("Notes:"));
}

#[test]
fn test_cli_memory_status_initializes_workspace_state_schema() {
    let temp = TempDir::new().unwrap();
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("--workspace")
        .arg(temp.path())
        .args(["memory", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Workspace schema: v1"));

    assert!(temp.path().join(".topagent/workspace-state.json").is_file());
    assert!(temp.path().join(".topagent/MEMORY.md").is_file());
    assert!(temp.path().join(".topagent/notes").is_dir());
    assert!(temp.path().join(".topagent/procedures").is_dir());
    assert!(temp.path().join(".topagent/trajectories").is_dir());
    assert!(temp.path().join(".topagent/exports/trajectories").is_dir());
}

#[test]
fn test_cli_run_status_reports_no_run_snapshot_for_fresh_workspace() {
    let temp = TempDir::new().unwrap();
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("--workspace")
        .arg(temp.path())
        .args(["run", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Run snapshot:"));
}

#[test]
fn test_readme_documents_uninstall() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent uninstall"));
    assert!(
        readme.contains("optionally remove binary")
            || readme.contains("remove binary")
            || readme.contains("installed binary")
    );
}

#[test]
fn test_readme_documents_product_install_commands() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent install"));
    assert!(readme.contains("topagent status"));
    assert!(readme.contains("topagent model status"));
    assert!(readme.contains("topagent model set <id>"));
    assert!(readme.contains("topagent model pick"));
    assert!(readme.contains("topagent memory status"));
    assert!(readme.contains("topagent procedure list"));
    assert!(readme.contains("topagent procedure show <id>"));
    assert!(readme.contains("topagent procedure prune"));
    assert!(readme.contains("topagent memory trajectory list"));
    assert!(readme.contains("topagent memory trajectory review <id>"));
    assert!(readme.contains("topagent memory trajectory export <id>"));
    assert!(readme.contains("topagent uninstall"));
    assert!(readme.contains("topagent service start"));
    assert!(readme.contains("topagent service stop"));
    assert!(readme.contains("topagent service restart"));
    assert!(readme.contains("topagent run status"));
    assert!(readme.contains("topagent run diff"));
    assert!(readme.contains("topagent run restore"));
    assert!(readme.contains("Download the latest release binary"));
}

#[test]
fn test_install_script_points_users_to_topagent_install() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();

    assert!(script.contains("$installed_bin install"));
    assert!(script.contains("$installed_bin status"));
    assert!(script.contains("Starting interactive TopAgent install"));
}

#[test]
fn test_install_script_prefers_release_assets() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();

    assert!(script.contains("TOPAGENT_INSTALL_RELEASE_BASE_URL"));
    assert!(script.contains("latest/download"));
    assert!(script.contains("x86_64-unknown-linux-gnu"));
}

#[test]
fn test_release_workflow_exists_and_uses_tag_trigger() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let workflow =
        std::fs::read_to_string(repo_root.join(".github/workflows/release.yml")).unwrap();

    assert!(workflow.contains("tags:"));
    assert!(workflow.contains("- \"v*\""));
    assert!(workflow.contains("softprops/action-gh-release"));
    assert!(workflow.contains("topagent-x86_64-unknown-linux-gnu"));
}

#[test]
fn test_operations_docs_cover_model_management() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    assert!(operations.contains("topagent model status"));
    assert!(operations.contains("topagent model set <model-id>"));
    assert!(operations.contains("topagent model pick"));
    assert!(operations.contains("topagent model refresh"));
    assert!(operations.contains("openrouter-models.json"));
}

#[test]
fn test_operations_docs_cover_run_snapshot_management() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    assert!(operations.contains("topagent run status"));
    assert!(operations.contains("topagent run diff"));
    assert!(operations.contains("topagent run restore"));
    assert!(operations.contains("topagent memory status"));
    assert!(operations.contains("topagent procedure list"));
    assert!(operations.contains("topagent procedure prune"));
    assert!(operations.contains("topagent memory trajectory review"));
    assert!(operations.contains("topagent memory trajectory export"));
    assert!(operations.contains(".topagent/run-snapshots"));
    assert!(operations.contains("clears persisted Telegram transcripts"));
}

#[test]
fn test_operations_docs_cover_current_workspace_state_schema() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    for required in [
        "workspace-state.json",
        "schema version `1`",
        "Hot-path prompt memory",
        "Evidence/export only",
        "Transport evidence",
    ] {
        assert!(
            operations.contains(required),
            "operations.md missing workspace-state contract phrase `{required}`"
        );
    }

    assert!(readme.contains(".topagent/workspace-state.json"));
    assert!(readme.contains(".topagent/notes/"));
}

#[test]
fn test_operations_docs_cover_lifecycle_sources_of_truth() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    for required in [
        "Runtime contract: `topagent config inspect`",
        "Install/service health: `topagent status`",
        "Run snapshots: `topagent run status`",
        "Workspace learning: `topagent memory status`",
        "Changing the configured model with `topagent model set <model-id>`",
        "run `topagent install`",
    ] {
        assert!(
            operations.contains(required),
            "operations.md missing lifecycle contract phrase `{required}`"
        );
    }
}

#[test]
fn test_readme_and_architecture_document_governed_learning_layers() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();
    let architecture = std::fs::read_to_string(repo_root.join("docs/architecture.md")).unwrap();

    assert!(readme.contains(".topagent/USER.md"));
    assert!(readme.contains("structured export records"));
    assert!(architecture.contains("Operator model"));
    assert!(architecture.contains("trajectory review/export"));
}

#[test]
fn test_cli_docs_consistency_readme_covers_all_subcommands() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    let subcommands = [
        "install",
        "status",
        "telegram",
        "service",
        "model",
        "memory",
        "procedure",
        "run",
        "config",
        "doctor",
        "upgrade",
        "uninstall",
    ];

    for cmd in &subcommands {
        assert!(
            readme.contains(&format!("topagent {}", cmd)),
            "README.md does not document `topagent {}`",
            cmd
        );
    }
}

#[test]
fn test_readme_bot_commands_table_agrees_with_bot_commands_truth() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    let bot_command_names = [
        "/start",
        "/help",
        "/stop",
        "/approvals",
        "/approve",
        "/deny",
        "/reset",
    ];

    for name in &bot_command_names {
        assert!(
            readme.contains(name),
            "README.md Bot commands table is missing `{}` \
             (must match BOT_COMMANDS in telegram/commands.rs)",
            name
        );
    }
}

#[test]
fn test_cli_help_uses_surface_constants_for_key_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    let help_output =
        String::from_utf8(cmd.arg("--help").assert().get_output().stdout.clone()).unwrap();

    assert!(
        help_output.contains("installation, service, and model status"),
        "status help should mention its purpose"
    );
    assert!(
        help_output.contains("health diagnostics"),
        "doctor help should mention its purpose"
    );
    assert!(
        help_output.contains("runtime contract"),
        "config inspect help should mention its purpose"
    );
    assert!(
        help_output.contains("workspace run snapshot"),
        "run subcommand help should mention run snapshot"
    );
}

#[test]
fn test_model_set_does_not_change_provider_is_documented() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    assert!(
        operations.contains("preserves the other managed values (including the provider)"),
        "operations.md must document that model set preserves the provider"
    );
    assert!(
        operations.contains("run `topagent install`"),
        "operations.md must document that provider change uses topagent install"
    );
}

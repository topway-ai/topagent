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
        .stdout(predicates::str::contains("Tool authoring:"));
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
fn test_cli_config_inspect_graceful_without_setup() {
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
fn test_cli_install_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("install"));
}

#[test]
fn test_cli_setup_alias_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("setup"));
}

#[test]
fn test_cli_status_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("status"));
}

#[test]
fn test_cli_service_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("service"));
}

#[test]
fn test_cli_model_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("model"));
}

#[test]
fn test_cli_memory_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("memory"));
}

#[test]
fn test_cli_procedure_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("procedure"));
}

#[test]
fn test_cli_trajectory_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("trajectory"));
}

#[test]
fn test_cli_checkpoint_appears_in_help() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("checkpoint"));
}

#[test]
fn test_cli_service_help_mentions_lifecycle_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["service", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("start"))
        .stdout(predicates::str::contains("stop"))
        .stdout(predicates::str::contains("restart"));
}

#[test]
fn test_cli_model_help_mentions_management_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["model", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status"))
        .stdout(predicates::str::contains("set"))
        .stdout(predicates::str::contains("pick"))
        .stdout(predicates::str::contains("list"))
        .stdout(predicates::str::contains("refresh"));
}

#[test]
fn test_cli_memory_help_mentions_status() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["memory", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status"));
}

#[test]
fn test_cli_memory_help_mentions_lint() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["memory", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("lint"))
        .stdout(predicates::str::contains("recall"));
}

#[test]
fn test_cli_memory_lint_clean_workspace_ok() {
    let temp = TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/topics")).unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/lessons")).unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/procedures")).unwrap();
    std::fs::create_dir_all(temp.path().join(".topagent/trajectories")).unwrap();
    std::fs::write(
        temp.path().join(".topagent/MEMORY.md"),
        "# TopAgent Memory Index\n\n- topic: arch | file: topics/arch.md | status: verified | note: layout\n",
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
fn test_cli_procedure_help_mentions_management_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["procedure", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("list"))
        .stdout(predicates::str::contains("show"))
        .stdout(predicates::str::contains("prune"))
        .stdout(predicates::str::contains("disable"));
}

#[test]
fn test_cli_trajectory_help_mentions_review_and_export_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["trajectory", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("list"))
        .stdout(predicates::str::contains("show"))
        .stdout(predicates::str::contains("review"))
        .stdout(predicates::str::contains("export"));
}

#[test]
fn test_cli_checkpoint_help_mentions_management_commands() {
    let mut cmd = Command::cargo_bin("topagent").unwrap();
    cmd.args(["checkpoint", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("status"))
        .stdout(predicates::str::contains("diff"))
        .stdout(predicates::str::contains("restore"));
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
        ));
}

#[test]
fn test_cli_checkpoint_status_reports_none_for_fresh_workspace() {
    let temp = TempDir::new().unwrap();
    let (_isolated, mut cmd) = isolated_topagent_command();
    cmd.arg("--workspace")
        .arg(temp.path())
        .args(["checkpoint", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "No active workspace checkpoint found.",
        ));
}

#[test]
fn test_readme_documents_uninstall() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent uninstall"));
    assert!(readme.contains("remove service, config, and installed binary"));
}

#[test]
fn test_readme_documents_product_setup_commands() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(readme.contains("topagent setup"));
    assert!(readme.contains("topagent install"));
    assert!(readme.contains("topagent status"));
    assert!(readme.contains("topagent model status"));
    assert!(readme.contains("topagent model set <id>"));
    assert!(readme.contains("topagent model pick"));
    assert!(readme.contains("topagent memory status"));
    assert!(readme.contains("topagent procedure list"));
    assert!(readme.contains("topagent procedure show <id>"));
    assert!(readme.contains("topagent procedure prune"));
    assert!(readme.contains("topagent trajectory list"));
    assert!(readme.contains("topagent trajectory review <id>"));
    assert!(readme.contains("topagent trajectory export <id>"));
    assert!(readme.contains("topagent uninstall"));
    assert!(readme.contains("topagent service start"));
    assert!(readme.contains("topagent service stop"));
    assert!(readme.contains("topagent service restart"));
    assert!(readme.contains("topagent checkpoint status"));
    assert!(readme.contains("topagent checkpoint diff"));
    assert!(readme.contains("topagent checkpoint restore"));
    assert!(readme.contains("Download the latest release binary"));
}

#[test]
fn test_install_script_output_no_longer_teaches_workspace_env() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();

    assert!(script.contains("cd /path/to/your/repo"));
    assert!(!script.contains("TOPAGENT_WORKSPACE"));
}

#[test]
fn test_install_script_points_users_to_topagent_install() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/install.sh")).unwrap();

    assert!(script.contains("$installed_bin install"));
    assert!(script.contains("$installed_bin status"));
    assert!(script.contains("Starting interactive TopAgent setup"));
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
fn test_operations_docs_explain_external_tool_sandbox_rollout() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    assert!(operations.contains("\"sandbox\": \"workspace\""));
    assert!(operations.contains("\"sandbox\": \"host\""));
    assert!(operations.contains("If `sandbox` is omitted, TopAgent rejects"));
    assert!(operations.contains("only supported workspace external-tool config file"));
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
fn test_operations_docs_cover_checkpoint_management() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let operations = std::fs::read_to_string(repo_root.join("docs/operations.md")).unwrap();

    assert!(operations.contains("topagent checkpoint status"));
    assert!(operations.contains("topagent checkpoint diff"));
    assert!(operations.contains("topagent checkpoint restore"));
    assert!(operations.contains("topagent memory status"));
    assert!(operations.contains("topagent procedure list"));
    assert!(operations.contains("topagent procedure prune"));
    assert!(operations.contains("topagent trajectory review"));
    assert!(operations.contains("topagent trajectory export"));
    assert!(operations.contains(".topagent/checkpoints"));
    assert!(operations.contains("clears persisted Telegram transcripts"));
}

#[test]
fn test_readme_and_architecture_document_governed_learning_layers() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();
    let architecture = std::fs::read_to_string(repo_root.join("docs/architecture.md")).unwrap();

    assert!(readme.contains(".topagent/USER.md"));
    assert!(readme.contains("reviewed and exported explicitly"));
    assert!(architecture.contains("Operator Model"));
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
        "trajectory",
        "checkpoint",
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
fn test_cli_docs_consistency_no_removed_commands_in_readme() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    assert!(
        !readme.contains("topagent observation"),
        "README.md still references removed `topagent observation` command"
    );
}

#[test]
fn test_telegram_bot_command_table_matches_router() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let readme = std::fs::read_to_string(repo_root.join("README.md")).unwrap();

    let bot_commands = [
        "/start",
        "/help",
        "/stop",
        "/approvals",
        "/approve",
        "/deny",
        "/reset",
    ];
    for cmd in &bot_commands {
        assert!(
            readme.contains(cmd),
            "README.md Bot commands table is missing `{}`",
            cmd
        );
    }
}

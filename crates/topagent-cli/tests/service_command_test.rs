use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct ServiceHarness {
    _root: TempDir,
    install_dir: PathBuf,
    binary_path: PathBuf,
    config_home: PathBuf,
    home: PathBuf,
    bin_dir: PathBuf,
    systemctl_root: PathBuf,
}

impl ServiceHarness {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let install_dir = root.path().join("install");
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let bin_dir = root.path().join("bin");
        let systemctl_root = root.path().join("fake-systemctl");

        fs::create_dir_all(&install_dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&systemctl_root).unwrap();

        let compiled_bin = Command::cargo_bin("topagent")
            .unwrap()
            .get_program()
            .to_owned();
        let binary_path = install_dir.join("topagent");
        let binary_contents = fs::read(compiled_bin).unwrap();
        fs::write(&binary_path, binary_contents).unwrap();
        make_executable(&binary_path);

        let systemctl_path = bin_dir.join("systemctl");
        fs::write(&systemctl_path, fake_systemctl_script()).unwrap();
        make_executable(&systemctl_path);

        Self {
            _root: root,
            install_dir,
            binary_path,
            config_home,
            home,
            bin_dir,
            systemctl_root,
        }
    }

    fn unit_path(&self) -> PathBuf {
        self.config_home
            .join("systemd")
            .join("user")
            .join("topagent-telegram.service")
    }

    fn env_path(&self) -> PathBuf {
        self.config_home
            .join("topagent")
            .join("services")
            .join("topagent-telegram.env")
    }

    fn workspace_path(&self) -> PathBuf {
        self.install_dir.join("workspace")
    }

    fn cache_path(&self) -> PathBuf {
        self.config_home
            .join("topagent")
            .join("cache")
            .join("openrouter-models.json")
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary_path);
        cmd.current_dir(&self.home)
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("TELEGRAM_BOT_TOKEN")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("FAKE_SYSTEMCTL_ROOT", &self.systemctl_root)
            .env("TOPAGENT_DISABLE_OPENROUTER_MODEL_FETCH", "1")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        cmd
    }

    fn calls_log(&self) -> String {
        fs::read_to_string(self.systemctl_root.join("calls.log")).unwrap_or_default()
    }
}

fn fake_systemctl_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "--user" ]]; then
  echo "expected --user" >&2
  exit 1
fi
shift

state_dir="${FAKE_SYSTEMCTL_ROOT:?}"
mkdir -p "$state_dir"
calls="$state_dir/calls.log"
enabled_file="$state_dir/enabled"
active_file="$state_dir/active"
result_file="$state_dir/result"
exec_file="$state_dir/exec_main_status"
unit_path="${XDG_CONFIG_HOME:?}/systemd/user/topagent-telegram.service"

cmd="${1:-}"
shift || true

case "$cmd" in
  show-environment)
    echo "XDG_CONFIG_HOME=${XDG_CONFIG_HOME}"
    ;;
  daemon-reload)
    echo "daemon-reload" >> "$calls"
    ;;
  enable)
    echo "enable $*" >> "$calls"
    echo "enabled" > "$enabled_file"
    echo "active" > "$active_file"
    echo "success" > "$result_file"
    echo "0" > "$exec_file"
    ;;
  start)
    echo "start $*" >> "$calls"
    echo "active" > "$active_file"
    echo "success" > "$result_file"
    echo "0" > "$exec_file"
    ;;
  stop)
    echo "stop $*" >> "$calls"
    echo "inactive" > "$active_file"
    ;;
  restart)
    echo "restart $*" >> "$calls"
    if [[ "${FAKE_SYSTEMCTL_FAIL_RESTART:-}" == "1" ]]; then
      echo "simulated restart failure" >&2
      exit 1
    fi
    echo "active" > "$active_file"
    echo "success" > "$result_file"
    echo "0" > "$exec_file"
    ;;
  disable)
    echo "disable $*" >> "$calls"
    echo "disabled" > "$enabled_file"
    ;;
  show)
    echo "show $*" >> "$calls"
    load_state="not-found"
    unit_state="disabled"
    active_state="inactive"
    sub_state="dead"
    result="success"
    exec_status="0"
    fragment_path=""

    if [[ -f "$unit_path" ]]; then
      load_state="loaded"
      fragment_path="$unit_path"
    fi
    if [[ -f "$enabled_file" ]]; then
      unit_state="$(cat "$enabled_file")"
    fi
    if [[ -f "$active_file" ]]; then
      active_state="$(cat "$active_file")"
    fi
    if [[ -f "$result_file" ]]; then
      result="$(cat "$result_file")"
    fi
    if [[ -f "$exec_file" ]]; then
      exec_status="$(cat "$exec_file")"
    fi
    if [[ "$active_state" == "active" ]]; then
      sub_state="running"
    elif [[ "$active_state" == "failed" ]]; then
      sub_state="failed"
    fi

    cat <<EOF
LoadState=$load_state
UnitFileState=$unit_state
ActiveState=$active_state
SubState=$sub_state
FragmentPath=$fragment_path
Result=$result
ExecMainStatus=$exec_status
EOF
    ;;
  *)
    echo "unsupported systemctl command: $cmd $*" >&2
    exit 1
    ;;
esac
"#
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

#[test]
fn test_install_prompts_creates_install_adjacent_workspace_and_starts_service() {
    let harness = ServiceHarness::new();
    let assert = harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("OpenRouter API key:"));
    assert!(stdout.contains("OpenRouter model:"));
    assert!(stdout.contains("Telegram bot token:"));
    assert!(stdout.contains("TopAgent installed."));
    assert!(stdout.contains("Service action: enabled and started"));
    assert!(stdout.contains(&harness.workspace_path().display().to_string()));
    assert!(stdout.contains("Open a private chat with your bot"));
    assert!(stdout.contains("topagent status"));

    assert!(harness.workspace_path().is_dir());
    assert_eq!(
        harness.workspace_path().canonicalize().unwrap(),
        harness
            .install_dir
            .join("workspace")
            .canonicalize()
            .unwrap()
    );

    let unit = fs::read_to_string(harness.unit_path()).unwrap();
    assert!(unit.contains("EnvironmentFile="));
    assert!(unit.contains("ExecStart="));
    assert!(unit.contains("telegram"));
    assert!(unit.contains(&harness.workspace_path().display().to_string()));
    assert!(!unit.contains("--model"));
    assert!(!unit.contains("--tool-authoring"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TELEGRAM_BOT_TOKEN="));
    assert!(env.contains("OPENROUTER_API_KEY="));
    assert!(env.contains("TOPAGENT_SERVICE_MANAGED=1"));
    assert!(env.contains("TOPAGENT_MODEL=\"minimax/minimax-m2.7\""));
    assert!(env.contains(&harness.workspace_path().display().to_string()));
    assert!(env.contains("TOPAGENT_MAX_STEPS=\"50\""));
    assert!(env.contains("TOPAGENT_MAX_RETRIES=\"3\""));
    assert!(env.contains("TOPAGENT_TIMEOUT_SECS=\"120\""));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(harness.env_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let calls = harness.calls_log();
    assert!(calls.contains("daemon-reload"));
    assert!(calls.contains("enable --now topagent-telegram.service"));
}

#[test]
fn test_install_uses_curated_fallback_model_list_and_persists_selected_model() {
    let harness = ServiceHarness::new();
    let assert = harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n2\n123456:abcdef\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("Using a starter model list."));
    assert!(stdout.contains("Select OpenRouter model"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_MODEL=\"qwen/qwen3.6-plus\""));

    let status = harness.command().arg("status").output().unwrap();
    assert!(status.status.success());
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status_stdout.contains("Configured default model: qwen/qwen3.6-plus"));
    assert!(status_stdout.contains("Effective model: qwen/qwen3.6-plus (persisted default)"));
}

#[test]
fn test_install_reuses_existing_config_when_prompt_is_left_blank() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let assert = harness
        .command()
        .arg("install")
        .write_stdin("\n\n\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("press Enter to keep the current value"));
    assert!(stdout.contains("Service action: enabled and restarted with updated config"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("test-openrouter-key"));
    assert!(env.contains("123456:abcdef"));

    let calls = harness.calls_log();
    assert_eq!(calls.matches("daemon-reload").count(), 2);
    assert_eq!(
        calls
            .matches("enable --now topagent-telegram.service")
            .count(),
        1
    );
    assert_eq!(calls.matches("enable topagent-telegram.service").count(), 1);
    assert_eq!(
        calls.matches("restart topagent-telegram.service").count(),
        1
    );
}

#[test]
fn test_install_persists_explicit_tool_authoring_mode() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--tool-authoring", "on", "install"])
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let unit = fs::read_to_string(harness.unit_path()).unwrap();
    assert!(!unit.contains("--tool-authoring"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_TOOL_AUTHORING=\"1\""));

    let status = harness.command().arg("status").output().unwrap();
    assert!(status.status.success());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("Tool authoring: on"));
}

#[test]
fn test_reinstall_preserves_existing_model_and_runtime_settings_when_flags_are_omitted() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args([
            "--model",
            "anthropic/claude-3.7-sonnet",
            "--max-steps",
            "77",
            "--max-retries",
            "9",
            "--timeout-secs",
            "45",
            "--tool-authoring",
            "on",
            "install",
        ])
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    harness
        .command()
        .arg("install")
        .write_stdin("\n\n\n")
        .assert()
        .success();

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_MODEL=\"anthropic/claude-3.7-sonnet\""));
    assert!(env.contains("TOPAGENT_MAX_STEPS=\"77\""));
    assert!(env.contains("TOPAGENT_MAX_RETRIES=\"9\""));
    assert!(env.contains("TOPAGENT_TIMEOUT_SECS=\"45\""));
    assert!(env.contains("TOPAGENT_TOOL_AUTHORING=\"1\""));
}

#[test]
fn test_status_reports_setup_and_service_health_after_install() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness.command().arg("status").output().unwrap();
    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Setup installed: yes"));
    assert!(stdout.contains("Service installed: yes"));
    assert!(stdout.contains("Enabled: yes"));
    assert!(stdout.contains("Running: yes"));
    assert!(stdout.contains("Service: topagent-telegram.service"));
    assert!(stdout.contains(&harness.env_path().display().to_string()));
    assert!(stdout.contains(&harness.workspace_path().display().to_string()));
}

#[test]
fn test_status_reports_unhealthy_hint_when_service_is_installed_but_failed() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    fs::write(harness.systemctl_root.join("active"), "failed").unwrap();
    fs::write(harness.systemctl_root.join("result"), "exit-code").unwrap();
    fs::write(harness.systemctl_root.join("exec_main_status"), "1").unwrap();

    let output = harness.command().arg("status").output().unwrap();
    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Running: no"));
    assert!(stdout.contains("Hint: service last result was exit-code"));
    assert!(stdout.contains("Exit status: 1"));
    assert!(stdout.contains("Inspect logs: journalctl --user -u topagent-telegram.service"));
}

#[test]
fn test_model_status_reports_configured_model() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "qwen/qwen3.6-plus:free", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .args(["model", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "model status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Configured default model: qwen/qwen3.6-plus:free"));
    assert!(stdout.contains("Effective model: qwen/qwen3.6-plus:free (persisted default)"));
    assert!(stdout.contains("Setup installed: yes"));
    assert!(stdout.contains("Service installed: yes"));
}

#[test]
fn test_model_set_preserves_other_env_values_restarts_service_and_updates_status() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "minimax/minimax-m2.7", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    let mut env = fs::read_to_string(harness.env_path()).unwrap();
    env.push_str("EXTRA_FLAG=\"still-here\"\n");
    fs::write(harness.env_path(), env).unwrap();

    let output = harness
        .command()
        .args(["model", "set", "qwen/qwen3.6-plus:free"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "model set should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("TopAgent model updated."));
    assert!(stdout.contains("Previous model: minimax/minimax-m2.7"));
    assert!(stdout.contains("Configured model: qwen/qwen3.6-plus:free"));
    assert!(stdout.contains("Service restart: yes"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("OPENROUTER_API_KEY=\"test-openrouter-key\""));
    assert!(env.contains("TELEGRAM_BOT_TOKEN=\"123456:abcdef\""));
    assert!(env.contains("TOPAGENT_MODEL=\"qwen/qwen3.6-plus:free\""));
    assert!(env.contains("TOPAGENT_MAX_STEPS=\"50\""));
    assert!(env.contains("EXTRA_FLAG=\"still-here\""));

    let status = harness.command().arg("status").output().unwrap();
    assert!(status.status.success());
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status_stdout.contains("Configured default model: qwen/qwen3.6-plus:free"));
    assert!(status_stdout.contains("Effective model: qwen/qwen3.6-plus:free (persisted default)"));

    let calls = harness.calls_log();
    assert!(calls.contains("restart topagent-telegram.service"));
}

#[test]
fn test_reinstall_restarts_service_and_status_reads_updated_model_from_single_config_path() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let reinstall = harness
        .command()
        .args(["--model", "anthropic/claude-sonnet-4.6", "install"])
        .write_stdin("\n\n")
        .output()
        .unwrap();
    assert!(
        reinstall.status.success(),
        "reinstall should succeed: {}",
        String::from_utf8_lossy(&reinstall.stderr)
    );
    let reinstall_stdout = String::from_utf8_lossy(&reinstall.stdout);
    assert!(reinstall_stdout.contains("Service action: enabled and restarted with updated config"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_MODEL=\"anthropic/claude-sonnet-4.6\""));

    let status = harness.command().arg("status").output().unwrap();
    assert!(status.status.success());
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status_stdout
        .contains("Configured default model: anthropic/claude-sonnet-4.6 (persisted default)"));
    assert!(
        status_stdout.contains("Effective model: anthropic/claude-sonnet-4.6 (persisted default)")
    );

    let model_status = harness
        .command()
        .args(["model", "status"])
        .output()
        .unwrap();
    assert!(model_status.status.success());
    let model_stdout = String::from_utf8_lossy(&model_status.stdout);
    assert!(model_stdout
        .contains("Configured default model: anthropic/claude-sonnet-4.6 (persisted default)"));
    assert!(
        model_stdout.contains("Effective model: anthropic/claude-sonnet-4.6 (persisted default)")
    );

    let calls = harness.calls_log();
    assert_eq!(
        calls
            .matches("enable --now topagent-telegram.service")
            .count(),
        1
    );
    assert_eq!(calls.matches("enable topagent-telegram.service").count(), 1);
    assert_eq!(
        calls.matches("restart topagent-telegram.service").count(),
        1
    );
}

#[test]
fn test_model_pick_updates_configured_model_and_restarts_service() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "minimax/minimax-m2.7", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    fs::create_dir_all(harness.cache_path().parent().unwrap()).unwrap();
    fs::write(
        harness.cache_path(),
        r#"{
  "updated_at_unix_secs": 4102444800,
  "models": [
    "qwen/qwen3.6-plus",
    "anthropic/claude-sonnet-4.6"
  ]
}"#,
    )
    .unwrap();

    let output = harness
        .command()
        .args(["model", "pick"])
        .write_stdin("1\n")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "model pick should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("TopAgent model updated."));
    assert!(stdout.contains("Previous model: minimax/minimax-m2.7"));
    assert!(stdout.contains("Configured model: anthropic/claude-sonnet-4.6"));
    assert!(stdout.contains("Selection source: interactive selection"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_MODEL=\"anthropic/claude-sonnet-4.6\""));

    let calls = harness.calls_log();
    assert!(calls.contains("restart topagent-telegram.service"));
}

#[test]
fn test_status_shows_effective_model_when_cli_override_is_present() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "qwen/qwen3.6-plus:free", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .args(["--model", "anthropic/claude-sonnet-4.6", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Configured default model: qwen/qwen3.6-plus:free"));
    assert!(stdout.contains("Effective model: anthropic/claude-sonnet-4.6 (CLI override)"));
}

#[test]
fn test_model_set_surfaces_restart_failure_after_updating_env() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "minimax/minimax-m2.7", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .env("FAKE_SYSTEMCTL_FAIL_RESTART", "1")
        .args(["model", "set", "qwen/qwen3.6-plus:free"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(
        "Updated the configured model from minimax/minimax-m2.7 to qwen/qwen3.6-plus:free"
    ));
    assert!(stderr.contains("failed to restart the TopAgent Telegram service"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TOPAGENT_MODEL=\"qwen/qwen3.6-plus:free\""));
}

#[test]
fn test_model_list_marks_current_model_when_cache_exists() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .args(["--model", "qwen/qwen3.6-plus", "install"])
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    fs::create_dir_all(harness.cache_path().parent().unwrap()).unwrap();
    fs::write(
        harness.cache_path(),
        r#"{
  "updated_at_unix_secs": 4102444800,
  "models": [
    "qwen/qwen3.6-plus",
    "anthropic/claude-sonnet-4.6"
  ]
}"#,
    )
    .unwrap();

    let output = harness.command().args(["model", "list"]).output().unwrap();
    assert!(
        output.status.success(),
        "model list should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("qwen/qwen3.6-plus (current)"));
    assert!(stdout.contains("anthropic/claude-sonnet-4.6"));
}

#[test]
fn test_model_list_without_cache_prints_refresh_hint() {
    let harness = ServiceHarness::new();
    let output = harness.command().args(["model", "list"]).output().unwrap();
    assert!(
        output.status.success(),
        "model list should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No cached OpenRouter model list found."));
    assert!(stdout.contains("topagent model refresh"));
}

#[test]
fn test_uninstall_stops_service_removes_managed_files_and_preserves_workspace() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness.command().arg("uninstall").output().unwrap();
    assert!(
        output.status.success(),
        "uninstall should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Stopped: yes"));
    assert!(stdout.contains("Disabled: yes"));
    assert!(stdout.contains("unit file"));
    assert!(stdout.contains("env file"));
    assert!(stdout.contains("installed binary"));
    assert!(stdout.contains("workspace directory preserved"));

    assert!(!harness.unit_path().exists());
    assert!(!harness.env_path().exists());
    assert!(!harness.binary_path.exists());
    assert!(harness.workspace_path().exists());

    let calls = harness.calls_log();
    assert!(calls.contains("stop topagent-telegram.service"));
    assert!(calls.contains("disable topagent-telegram.service"));
}

#[test]
fn test_service_start_stop_and_restart_control_the_installed_service() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let stop = harness
        .command()
        .args(["service", "stop"])
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "service stop should succeed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let stop_stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(stop_stdout.contains("TopAgent service stopped"));
    assert!(stop_stdout.contains("topagent service start"));

    let start = harness
        .command()
        .args(["service", "start"])
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "service start should succeed: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    let start_stdout = String::from_utf8_lossy(&start.stdout);
    assert!(start_stdout.contains("TopAgent service started"));
    assert!(start_stdout.contains("topagent service stop"));

    let restart = harness
        .command()
        .args(["service", "restart"])
        .output()
        .unwrap();
    assert!(
        restart.status.success(),
        "service restart should succeed: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    let restart_stdout = String::from_utf8_lossy(&restart.stdout);
    assert!(restart_stdout.contains("TopAgent service restarted"));
    assert!(restart_stdout.contains("topagent status"));
    assert_eq!(restart_stdout.matches("  topagent status").count(), 1);

    let calls = harness.calls_log();
    assert!(calls.contains("stop topagent-telegram.service"));
    assert!(calls.contains("start topagent-telegram.service"));
    assert!(calls.contains("restart topagent-telegram.service"));
}

// ── Scenario: full install → model-set → status lifecycle chain ──

#[test]
fn test_install_then_model_set_then_status_lifecycle_preserves_all_env_values() {
    let harness = ServiceHarness::new();

    // Step 1: Install with explicit runtime settings
    harness
        .command()
        .args([
            "--model",
            "minimax/minimax-m2.7",
            "--max-steps",
            "42",
            "--timeout-secs",
            "90",
            "--tool-authoring",
            "on",
            "install",
        ])
        .write_stdin("scenario-api-key\n123456:scenario-token\n")
        .assert()
        .success();

    let env_after_install = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env_after_install.contains("TOPAGENT_MODEL=\"minimax/minimax-m2.7\""));
    assert!(env_after_install.contains("TOPAGENT_MAX_STEPS=\"42\""));
    assert!(env_after_install.contains("TOPAGENT_TIMEOUT_SECS=\"90\""));
    assert!(env_after_install.contains("TOPAGENT_TOOL_AUTHORING=\"1\""));

    // Step 2: Change model — all other env values must survive
    let model_set = harness
        .command()
        .args(["model", "set", "anthropic/claude-sonnet-4.6"])
        .output()
        .unwrap();
    assert!(model_set.status.success());
    let model_set_stdout = String::from_utf8_lossy(&model_set.stdout);
    assert!(model_set_stdout.contains("Previous model: minimax/minimax-m2.7"));
    assert!(model_set_stdout.contains("Configured model: anthropic/claude-sonnet-4.6"));

    let env_after_set = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env_after_set.contains("TOPAGENT_MODEL=\"anthropic/claude-sonnet-4.6\""));
    assert!(env_after_set.contains("OPENROUTER_API_KEY=\"scenario-api-key\""));
    assert!(env_after_set.contains("TELEGRAM_BOT_TOKEN=\"123456:scenario-token\""));
    assert!(env_after_set.contains("TOPAGENT_MAX_STEPS=\"42\""));
    assert!(env_after_set.contains("TOPAGENT_TIMEOUT_SECS=\"90\""));
    assert!(env_after_set.contains("TOPAGENT_TOOL_AUTHORING=\"1\""));

    // Step 3: Status reflects the updated model and preserved settings
    let status = harness.command().arg("status").output().unwrap();
    assert!(status.status.success());
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status_stdout.contains("Configured default model: anthropic/claude-sonnet-4.6"));
    assert!(
        status_stdout.contains("Effective model: anthropic/claude-sonnet-4.6 (persisted default)")
    );
    assert!(status_stdout.contains("Tool authoring: on"));

    // Step 4: Model status agrees
    let model_status = harness
        .command()
        .args(["model", "status"])
        .output()
        .unwrap();
    assert!(model_status.status.success());
    let model_stdout = String::from_utf8_lossy(&model_status.stdout);
    assert!(model_stdout.contains("Configured default model: anthropic/claude-sonnet-4.6"));

    // Step 5: Verify systemctl call chain
    let calls = harness.calls_log();
    assert!(calls.contains("daemon-reload"));
    assert!(calls.contains("enable --now topagent-telegram.service"));
    assert!(calls.contains("restart topagent-telegram.service"));
}

// ── Scenario: checkpoint status and diff on fresh workspace ──

#[test]
fn test_checkpoint_status_on_fresh_workspace_reports_no_checkpoint() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .args(["--workspace", &harness.workspace_path().display().to_string()])
        .args(["checkpoint", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "checkpoint status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Checkpoint: none"));
    assert!(stdout.contains("No active workspace checkpoint found"));
}

#[test]
fn test_checkpoint_diff_on_fresh_workspace_reports_no_checkpoint() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .args(["--workspace", &harness.workspace_path().display().to_string()])
        .args(["checkpoint", "diff"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "checkpoint diff should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No active workspace checkpoint found"));
}

// ── Scenario: reinstall with changed model updates env atomically ──

#[test]
fn test_reinstall_with_new_model_replaces_old_model_atomically_in_env() {
    let harness = ServiceHarness::new();

    // First install with model A
    harness
        .command()
        .args(["--model", "model-a/original", "install"])
        .write_stdin("key-a\n123456:token-a\n")
        .assert()
        .success();

    let env1 = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env1.contains("TOPAGENT_MODEL=\"model-a/original\""));
    assert_eq!(
        env1.matches("TOPAGENT_MODEL=").count(),
        1,
        "env should have exactly one TOPAGENT_MODEL line after first install"
    );

    // Reinstall with model B, keeping secrets
    harness
        .command()
        .args(["--model", "model-b/replacement", "install"])
        .write_stdin("\n\n")
        .assert()
        .success();

    let env2 = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env2.contains("TOPAGENT_MODEL=\"model-b/replacement\""));
    assert!(!env2.contains("model-a/original"));
    assert_eq!(
        env2.matches("TOPAGENT_MODEL=").count(),
        1,
        "env should have exactly one TOPAGENT_MODEL line after reinstall"
    );
    // Secrets carried over
    assert!(env2.contains("OPENROUTER_API_KEY=\"key-a\""));
    assert!(env2.contains("TELEGRAM_BOT_TOKEN=\"123456:token-a\""));
}

// ── Scenario: memory status on installed workspace shows all learning layers ──

#[test]
fn test_memory_status_after_install_shows_all_learning_layers() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-key\n\n123456:abcdef\n")
        .assert()
        .success();

    let output = harness
        .command()
        .args(["--workspace", &harness.workspace_path().display().to_string()])
        .args(["memory", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "memory status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Operator model"));
    assert!(stdout.contains("Workspace index"));
    assert!(stdout.contains("Procedures: 0"));
    assert!(stdout.contains("Lessons: 0"));
    assert!(stdout.contains("Observations: 0"));
}

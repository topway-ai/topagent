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

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary_path);
        cmd.current_dir(&self.home)
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("TELEGRAM_BOT_TOKEN")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("FAKE_SYSTEMCTL_ROOT", &self.systemctl_root)
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
  stop)
    echo "stop $*" >> "$calls"
    echo "inactive" > "$active_file"
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
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("OpenRouter API key:"));
    assert!(stdout.contains("Telegram bot token:"));
    assert!(stdout.contains("TopAgent installed."));
    assert!(stdout.contains("Started: yes"));
    assert!(stdout.contains(&harness.env_path().display().to_string()));
    assert!(stdout.contains(&harness.workspace_path().display().to_string()));
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

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TELEGRAM_BOT_TOKEN="));
    assert!(env.contains("OPENROUTER_API_KEY="));
    assert!(env.contains("TOPAGENT_SERVICE_MANAGED=1"));
    assert!(env.contains(&harness.workspace_path().display().to_string()));

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
fn test_install_reuses_existing_config_when_prompt_is_left_blank() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
        .assert()
        .success();

    let assert = harness
        .command()
        .arg("install")
        .write_stdin("\n\n")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("press Enter to keep the current value"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("test-openrouter-key"));
    assert!(env.contains("123456:abcdef"));

    let calls = harness.calls_log();
    assert_eq!(calls.matches("daemon-reload").count(), 2);
    assert_eq!(
        calls
            .matches("enable --now topagent-telegram.service")
            .count(),
        2
    );
}

#[test]
fn test_status_reports_setup_and_service_health_after_install() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
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
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
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
fn test_uninstall_stops_service_removes_managed_files_and_preserves_workspace() {
    let harness = ServiceHarness::new();
    harness
        .command()
        .arg("install")
        .write_stdin("test-openrouter-key\n123456:abcdef\n")
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
    assert!(stdout.contains("workspace directory preserved"));

    assert!(!harness.unit_path().exists());
    assert!(!harness.env_path().exists());
    assert!(harness.workspace_path().exists());

    let calls = harness.calls_log();
    assert!(calls.contains("stop topagent-telegram.service"));
    assert!(calls.contains("disable topagent-telegram.service"));
}

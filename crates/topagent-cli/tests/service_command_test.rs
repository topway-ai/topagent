use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct ServiceHarness {
    _root: TempDir,
    config_home: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    bin_dir: PathBuf,
    systemctl_root: PathBuf,
}

impl ServiceHarness {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let workspace = root.path().join("workspace");
        let bin_dir = root.path().join("bin");
        let systemctl_root = root.path().join("fake-systemctl");

        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&systemctl_root).unwrap();

        let systemctl_path = bin_dir.join("systemctl");
        fs::write(&systemctl_path, fake_systemctl_script()).unwrap();
        make_executable(&systemctl_path);

        Self {
            _root: root,
            config_home,
            home,
            workspace,
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

    fn workspace_str(&self) -> String {
        self.workspace.display().to_string()
    }

    fn command(&self) -> Command {
        let mut cmd = Command::cargo_bin("topagent").unwrap();
        cmd.current_dir(&self.workspace)
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

    fn install(&self) -> String {
        let output = self
            .command()
            .env("OPENROUTER_API_KEY", "test-openrouter-key")
            .args([
                "--workspace",
                &self.workspace_str(),
                "service",
                "install",
                "--token",
                "123456:abcdef",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "install should succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
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
fn test_service_install_writes_unit_and_env_and_starts_service() {
    let harness = ServiceHarness::new();
    let stdout = harness.install();

    assert!(stdout.contains("TopAgent Telegram service installed and started."));
    assert!(stdout.contains(&harness.unit_path().display().to_string()));
    assert!(stdout.contains(&harness.env_path().display().to_string()));
    assert!(stdout.contains("topagent service status"));

    let unit = fs::read_to_string(harness.unit_path()).unwrap();
    assert!(unit.contains("EnvironmentFile="));
    assert!(unit.contains("ExecStart="));
    assert!(unit.contains("telegram"));

    let env = fs::read_to_string(harness.env_path()).unwrap();
    assert!(env.contains("TELEGRAM_BOT_TOKEN="));
    assert!(env.contains("OPENROUTER_API_KEY="));
    assert!(env.contains("TOPAGENT_SERVICE_MANAGED=1"));

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
fn test_service_status_reports_installed_enabled_and_running() {
    let harness = ServiceHarness::new();
    harness.install();

    let output = harness
        .command()
        .args(["service", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Installed: yes"));
    assert!(stdout.contains("Enabled: yes"));
    assert!(stdout.contains("Running: yes"));
    assert!(stdout.contains(&harness.unit_path().display().to_string()));
    assert!(stdout.contains(&harness.workspace.display().to_string()));
    assert!(stdout.contains("Route: openrouter | minimax/minimax-m2.7"));
}

#[test]
fn test_service_status_reports_failure_hints_when_service_is_unhealthy() {
    let harness = ServiceHarness::new();
    harness.install();

    fs::write(harness.systemctl_root.join("active"), "failed").unwrap();
    fs::write(harness.systemctl_root.join("result"), "exit-code").unwrap();
    fs::write(harness.systemctl_root.join("exec_main_status"), "1").unwrap();

    let output = harness
        .command()
        .args(["service", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Running: no"));
    assert!(stdout.contains("Last result: exit-code"));
    assert!(stdout.contains("Exit status: 1"));
    assert!(stdout.contains("Inspect logs: journalctl --user -u topagent-telegram.service"));
}

#[test]
fn test_service_uninstall_stops_disables_and_removes_managed_files() {
    let harness = ServiceHarness::new();
    harness.install();

    let output = harness
        .command()
        .args(["service", "uninstall"])
        .output()
        .unwrap();
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

    assert!(!harness.unit_path().exists());
    assert!(!harness.env_path().exists());

    let calls = harness.calls_log();
    assert!(calls.contains("stop topagent-telegram.service"));
    assert!(calls.contains("disable topagent-telegram.service"));
}

#[test]
fn test_service_install_is_idempotent_for_managed_files() {
    let harness = ServiceHarness::new();
    harness.install();
    let stdout = harness.install();

    assert!(stdout.contains("installed and started"));
    let calls = harness.calls_log();
    assert_eq!(calls.matches("daemon-reload").count(), 2);
    assert_eq!(
        calls
            .matches("enable --now topagent-telegram.service")
            .count(),
        2
    );
}

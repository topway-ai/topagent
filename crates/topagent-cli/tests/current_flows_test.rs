use assert_cmd::Command;
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use topagent_core::run_snapshot::{RunSnapshotCaptureMetadata, RunSnapshotCaptureSource};
use topagent_core::WorkspaceRunSnapshotStore;

const MANAGED_HEADER: &str = "# Managed by TopAgent. Safe to remove with `topagent uninstall`.";

struct IsolatedTopagent {
    _temp: TempDir,
    home: PathBuf,
    config: PathBuf,
    workspace: PathBuf,
}

impl IsolatedTopagent {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let config = temp.path().join("config");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        Self {
            _temp: temp,
            home,
            config,
            workspace,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::cargo_bin("topagent").unwrap();
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(default_path),
            )
            .env("TOPAGENT_DISABLE_OPENROUTER_MODEL_FETCH", "1");
        cmd
    }

    fn service_env_path(&self) -> PathBuf {
        self.config.join("topagent/services/topagent-telegram.env")
    }

    fn service_unit_path(&self) -> PathBuf {
        self.config.join("systemd/user/topagent-telegram.service")
    }

    fn model_cache_path(&self) -> PathBuf {
        self.config.join("topagent/cache/openrouter-models.json")
    }

    fn fake_systemctl(&self) -> FakeSystemctl {
        FakeSystemctl::new(self._temp.path(), &self.service_unit_path())
    }

    fn write_managed_env(
        &self,
        provider: &str,
        model: &str,
        openrouter_key: Option<&str>,
        opencode_key: Option<&str>,
    ) {
        let path = self.service_env_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        body.push_str(MANAGED_HEADER);
        body.push('\n');
        body.push_str("TOPAGENT_SERVICE_MANAGED=\"1\"\n");
        body.push_str(&format!("TOPAGENT_PROVIDER={}\n", quote_env(provider)));
        body.push_str(&format!(
            "OPENROUTER_API_KEY={}\n",
            quote_env(openrouter_key.unwrap_or(""))
        ));
        if let Some(opencode_key) = opencode_key {
            body.push_str(&format!("OPENCODE_API_KEY={}\n", quote_env(opencode_key)));
        }
        body.push_str("TELEGRAM_BOT_TOKEN=\"123456:telegram-secret\"\n");
        body.push_str(&format!(
            "TOPAGENT_WORKSPACE={}\n",
            quote_env(&self.workspace.display().to_string())
        ));
        body.push_str(&format!("TOPAGENT_MODEL={}\n", quote_env(model)));
        body.push_str("TOPAGENT_MAX_STEPS=\"37\"\n");
        body.push_str("TOPAGENT_MAX_RETRIES=\"5\"\n");
        body.push_str("TOPAGENT_TIMEOUT_SECS=\"88\"\n");
        body.push_str("TELEGRAM_ALLOWED_DM_USERNAME=\"operator\"\n");
        body.push_str("TELEGRAM_BOUND_DM_USER_ID=\"424242\"\n");
        fs::write(path, body).unwrap();
    }

    fn write_managed_unit(&self) {
        let path = self.service_unit_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "{MANAGED_HEADER}\n[Unit]\nDescription=TopAgent test service\n[Service]\nExecStart=/tmp/topagent telegram\n"
            ),
        )
        .unwrap();
    }

    fn write_openrouter_cache(&self, models: &[&str]) {
        let path = self.model_cache_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let models_json = models
            .iter()
            .map(|model| format!("\"{}\"", model))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            path,
            format!(
                "{{\n  \"updated_at_unix_secs\": 1700000000,\n  \"models\": [{}]\n}}\n",
                models_json
            ),
        )
        .unwrap();
    }

    fn write_current_workspace_state(&self) {
        write_current_workspace_state(&self.workspace);
    }
}

struct FakeSystemctl {
    path: OsString,
    log_path: PathBuf,
    status_path: PathBuf,
}

impl FakeSystemctl {
    fn new(temp_root: &Path, fragment_path: &Path) -> Self {
        let bin_dir = temp_root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let log_path = temp_root.join("systemctl.log");
        let status_path = temp_root.join("systemctl-status.txt");
        let systemctl = bin_dir.join("systemctl");
        fs::write(
            &systemctl,
            format!(
                "#!/bin/sh\n\
                 set -eu\n\
                 printf '%s\\n' \"$*\" >> {}\n\
                 if [ \"$#\" -ge 2 ] && [ \"$1\" = \"--user\" ] && [ \"$2\" = \"show-environment\" ]; then\n\
                   exit 0\n\
                 fi\n\
                 if [ \"$#\" -ge 3 ] && [ \"$1\" = \"--user\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"topagent-telegram.service\" ]; then\n\
                   cat {}\n\
                   exit 0\n\
                 fi\n\
                 exit 0\n",
                shell_quote(&log_path.display().to_string()),
                shell_quote(&status_path.display().to_string()),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut path = bin_dir.into_os_string();
        path.push(":");
        path.push(default_path());

        let fake = Self {
            path,
            log_path,
            status_path,
        };
        fake.set_service_status(
            "loaded",
            "enabled",
            "active",
            "running",
            fragment_path,
            "success",
            "0",
        );
        fake
    }

    fn path(&self) -> OsString {
        self.path.clone()
    }

    fn set_service_status(
        &self,
        load_state: &str,
        unit_file_state: &str,
        active_state: &str,
        sub_state: &str,
        fragment_path: &Path,
        result: &str,
        exec_main_status: &str,
    ) {
        fs::write(
            &self.status_path,
            format!(
                "LoadState={load_state}\n\
                 UnitFileState={unit_file_state}\n\
                 ActiveState={active_state}\n\
                 SubState={sub_state}\n\
                 FragmentPath={}\n\
                 Result={result}\n\
                 ExecMainStatus={exec_main_status}\n",
                fragment_path.display()
            ),
        )
        .unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log_path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn default_path() -> OsString {
    OsString::from("/usr/bin:/bin")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn quote_env(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    )
}

fn write_current_workspace_state(workspace: &Path) {
    fs::create_dir_all(workspace.join(".topagent/notes")).unwrap();
    fs::create_dir_all(workspace.join(".topagent/procedures")).unwrap();
    fs::create_dir_all(workspace.join(".topagent/trajectories")).unwrap();
    fs::create_dir_all(workspace.join(".topagent/exports/trajectories")).unwrap();
    fs::write(
        workspace.join(".topagent/workspace-state.json"),
        "{\n  \"schema_version\": 1,\n  \"state_model\": \"topagent-workspace-state-v1\"\n}\n",
    )
    .unwrap();
    fs::write(
        workspace.join(".topagent/MEMORY.md"),
        "# TopAgent Memory Index\n\nKeep this file tiny.\n",
    )
    .unwrap();
}

fn assert_current_workspace_state_layout(workspace: &Path, expected_top_level: &[&str]) {
    let marker_path = workspace.join(".topagent/workspace-state.json");
    let marker: Value = serde_json::from_str(&fs::read_to_string(marker_path).unwrap()).unwrap();
    assert_eq!(marker["schema_version"], Value::from(1));
    assert_eq!(
        marker["state_model"],
        Value::from("topagent-workspace-state-v1")
    );

    for directory in [
        ".topagent/notes",
        ".topagent/procedures",
        ".topagent/trajectories",
        ".topagent/exports/trajectories",
    ] {
        assert!(
            workspace.join(directory).is_dir(),
            "missing current state directory {directory}"
        );
    }
    assert!(
        workspace.join(".topagent/MEMORY.md").is_file(),
        "missing current memory index"
    );

    let mut actual_top_level = fs::read_dir(workspace.join(".topagent"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    actual_top_level.sort();
    assert_eq!(actual_top_level, expected_top_level);
}

fn output_stdout(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

fn output_stderr(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).unwrap()
}

fn read_env_value(path: &Path, key: &str) -> Option<String> {
    let raw = fs::read_to_string(path).unwrap();
    raw.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        if candidate.trim() != key {
            return None;
        }
        Some(value.trim().trim_matches('"').to_string())
    })
}

fn write_procedure(path: &Path, status: &str) {
    fs::write(
        path,
        format!(
            "# Approval Mailbox Compaction Playbook\n\n\
             **Saved:** <t:1700000000>\n\
             **Status:** {status}\n\
             **When To Use:** Use for approval mailbox compaction and runtime routing work.\n\
             **Verification:** cargo test -p topagent-cli telegram\n\
             **Reuse Count:** 1\n\
             **Revision Count:** 0\n\
             **Source Task:** harden telegram routing\n\n\
             ---\n\n\
             ## Prerequisites\n\n\
             - Stay inside the workspace.\n\n\
             ## Steps\n\n\
             1. Inspect the command surface.\n\
             2. Patch the router seam.\n\
             3. Run focused tests.\n\n\
             ## Pitfalls\n\n\
             - Do not route unsupported slash commands as work.\n\n\
             ---\n\
             *Saved by topagent*\n"
        ),
    )
    .unwrap();
}

fn write_exportable_trajectory(path: &Path) {
    fs::write(
        path,
        r#"{
  "version": 1,
  "id": "trj-1700000000-approval-mailbox",
  "saved_at_unix_secs": 1700000000,
  "task_intent": "Repair approval mailbox workflow",
  "task_mode": "execute",
  "plan_summary": ["Inspect the workflow", "Patch routing", "Run verification"],
  "tool_sequence": [
    {"tool_name": "read", "summary": "read telegram/router.rs"},
    {"tool_name": "edit", "summary": "edit telegram/router.rs"},
    {"tool_name": "bash", "summary": "verification: cargo test -p topagent-cli telegram"}
  ],
  "changed_files": ["crates/topagent-cli/src/telegram/router.rs"],
  "verification": [
    {"command": "cargo test -p topagent-cli telegram", "exit_code": 0, "succeeded": true}
  ],
  "outcome_summary": "Router behavior was hardened and verified.",
  "note_file": ".topagent/notes/runtime.md",
  "procedure_file": ".topagent/procedures/1700000000-approval-mailbox.md",
  "redaction": {"secret_safe": true, "stored_outputs": false},
  "source_labels": [],
  "governance": {
    "review_state": "local_only",
    "reviewed_at_unix_secs": null,
    "exported_at_unix_secs": null,
    "exported_file": null
  }
}
"#,
    )
    .unwrap();
}

#[test]
fn installed_runtime_contract_agrees_across_config_status_and_doctor() {
    let env = IsolatedTopagent::new();
    env.write_current_workspace_state();
    env.write_managed_env("Opencode", "glm-5.1", None, Some("opencode-secret"));
    env.write_managed_unit();
    let systemctl = env.fake_systemctl();
    assert_current_workspace_state_layout(
        &env.workspace,
        &[
            "MEMORY.md",
            "exports",
            "notes",
            "procedures",
            "trajectories",
            "workspace-state.json",
        ],
    );

    let config_output = output_stdout(env.command().args(["config", "inspect"]).assert().success());
    assert!(config_output.contains("Provider:           Opencode"));
    assert!(config_output.contains("Model:              glm-5.1"));
    assert!(config_output.contains("Opencode:         present"));
    assert!(config_output.contains("OpenRouter:       missing"));
    assert!(config_output.contains("Bot token:        present"));
    assert!(config_output.contains("DM access:        restricted to @operator (bound)"));
    assert!(config_output.contains("Max steps:        37"));
    assert!(!config_output.contains("opencode-secret"));
    assert!(!config_output.contains("telegram-secret"));

    let status_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .arg("status")
            .assert()
            .success(),
    );
    assert!(status_output.contains("Installation present: yes"));
    assert!(status_output.contains("Service installed: yes"));
    assert!(status_output.contains("Enabled: yes"));
    assert!(status_output.contains("Running: yes"));
    assert!(status_output.contains("Configured default model: glm-5.1 (persisted default)"));
    assert!(status_output.contains("Effective model: glm-5.1 (persisted default)"));
    assert!(status_output.contains("Last state: active (running)"));
    assert!(status_output.contains("topagent config inspect"));
    assert!(status_output.contains("topagent run status"));

    let doctor_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .arg("doctor")
            .assert()
            .success(),
    );
    assert!(doctor_output.contains("[OK] Opencode API key: present (managed config)"));
    assert!(doctor_output
        .contains("[OK] managed service files: unit file and env file installed and managed"));
    assert!(doctor_output.contains("[OK] workspace layout: schema v1"));
    assert!(!doctor_output.contains("[ERROR] OpenRouter API key"));
}

#[test]
fn model_lifecycle_preserves_provider_and_surfaces_current_contract() {
    let env = IsolatedTopagent::new();
    env.write_current_workspace_state();
    env.write_managed_env(
        "OpenRouter",
        "anthropic/claude-sonnet-4.6",
        Some("openrouter-secret"),
        None,
    );
    env.write_managed_unit();
    env.write_openrouter_cache(&["glm-4", "anthropic/claude-sonnet-4.6"]);
    let systemctl = env.fake_systemctl();

    let before = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["model", "status"])
            .assert()
            .success(),
    );
    assert!(before.contains("Configured default model: anthropic/claude-sonnet-4.6 [OpenRouter]"));
    assert!(before.contains("Service installed: yes"));
    assert!(before.contains("Cached models: 2"));

    let set_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["model", "set", "glm-4"])
            .assert()
            .success(),
    );
    assert!(set_output.contains("Previous model: anthropic/claude-sonnet-4.6"));
    assert!(set_output.contains("Configured model: glm-4 [OpenRouter]"));
    assert!(set_output.contains("Service restart: yes"));

    let env_path = env.service_env_path();
    assert_eq!(
        read_env_value(&env_path, "TOPAGENT_PROVIDER").as_deref(),
        Some("OpenRouter")
    );
    assert_eq!(
        read_env_value(&env_path, "TOPAGENT_MODEL").as_deref(),
        Some("glm-4")
    );
    assert_eq!(
        read_env_value(&env_path, "OPENROUTER_API_KEY").as_deref(),
        Some("openrouter-secret")
    );

    let contract_output =
        output_stdout(env.command().args(["config", "inspect"]).assert().success());
    assert!(contract_output.contains("Provider:           OpenRouter"));
    assert!(contract_output.contains("Model:              glm-4"));
    assert!(!contract_output.contains("openrouter-secret"));

    let list_output = output_stdout(env.command().args(["model", "list"]).assert().success());
    assert!(list_output.contains("glm-4 (current)"));
    assert!(list_output.contains("anthropic/claude-sonnet-4.6"));

    let refresh_output = output_stdout(env.command().args(["model", "refresh"]).assert().success());
    assert!(refresh_output.contains("Live OpenRouter model refresh failed"));
    assert!(refresh_output.contains("Keeping cached models"));

    let pick_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["--model", "openrouter/picked-model", "model", "pick"])
            .assert()
            .success(),
    );
    assert!(pick_output.contains("Model: openrouter/picked-model (--model)"));
    assert!(pick_output.contains("Previous model: glm-4"));
    assert!(pick_output.contains("Configured model: openrouter/picked-model [OpenRouter]"));
    assert!(pick_output.contains("Selection source: CLI override"));
    assert!(pick_output.contains("Service restart: yes"));
    assert_eq!(
        read_env_value(&env_path, "TOPAGENT_PROVIDER").as_deref(),
        Some("OpenRouter")
    );
    assert_eq!(
        read_env_value(&env_path, "TOPAGENT_MODEL").as_deref(),
        Some("openrouter/picked-model")
    );
    assert_eq!(
        read_env_value(&env_path, "OPENROUTER_API_KEY").as_deref(),
        Some("openrouter-secret")
    );

    let picked_contract =
        output_stdout(env.command().args(["config", "inspect"]).assert().success());
    assert!(picked_contract.contains("Provider:           OpenRouter"));
    assert!(picked_contract.contains("Model:              openrouter/picked-model"));
    assert!(!picked_contract.contains("openrouter-secret"));

    let restart_calls = systemctl
        .calls()
        .into_iter()
        .filter(|call| call == "--user restart topagent-telegram.service")
        .count();
    assert_eq!(restart_calls, 2, "expected one restart per model change");
}

#[test]
fn run_snapshot_cli_flow_reports_diffs_restores_files_and_clears_transcripts() {
    let env = IsolatedTopagent::new();
    write_current_workspace_state(&env.workspace);
    fs::write(env.workspace.join("tracked.txt"), "before\n").unwrap();
    fs::write(
        env.workspace.join(".topagent/notes/learning.md"),
        "# Learning\nkeep this durable note\n",
    )
    .unwrap();
    fs::create_dir_all(env.workspace.join(".topagent/procedures")).unwrap();
    fs::write(
        env.workspace.join(".topagent/procedures/playbook.md"),
        "# Playbook\nkeep this procedure\n",
    )
    .unwrap();

    let store = WorkspaceRunSnapshotStore::new(env.workspace.clone());
    store
        .capture_file(
            "tracked.txt",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "structured write"),
        )
        .unwrap();
    fs::write(env.workspace.join("tracked.txt"), "after\n").unwrap();
    fs::create_dir_all(env.workspace.join(".topagent/telegram-history")).unwrap();
    fs::write(
        env.workspace
            .join(".topagent/telegram-history/chat-42.json"),
        "{}",
    )
    .unwrap();

    let status_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "status"])
            .assert()
            .success(),
    );
    assert!(status_output.contains("Run snapshot:           present"));
    assert!(status_output.contains("Captured paths:     1"));
    assert!(status_output.contains("Telegram transcripts: 1 chat file"));
    assert_current_workspace_state_layout(
        &env.workspace,
        &[
            "MEMORY.md",
            "exports",
            "notes",
            "procedures",
            "run-snapshots",
            "telegram-history",
            "trajectories",
            "workspace-state.json",
        ],
    );

    let diff_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "diff"])
            .assert()
            .success(),
    );
    assert!(diff_output.contains("Run snapshot:"));
    assert!(diff_output.contains("workspace/"));
    assert!(diff_output.contains("after"));

    let restore_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "restore"])
            .assert()
            .success(),
    );
    assert!(restore_output.contains("Restored files: 1"));
    assert!(restore_output.contains("- restored tracked.txt"));
    assert!(restore_output.contains("Cleared persisted Telegram transcripts"));
    assert_eq!(
        fs::read_to_string(env.workspace.join("tracked.txt")).unwrap(),
        "before\n"
    );
    assert!(!env.workspace.join(".topagent/telegram-history").exists());
    assert_eq!(
        fs::read_to_string(env.workspace.join(".topagent/notes/learning.md")).unwrap(),
        "# Learning\nkeep this durable note\n"
    );
    assert_current_workspace_state_layout(
        &env.workspace,
        &[
            "MEMORY.md",
            "exports",
            "notes",
            "procedures",
            "run-snapshots",
            "trajectories",
            "workspace-state.json",
        ],
    );
    assert!(store.latest_status().unwrap().is_none());
}

#[test]
fn workspace_learning_flow_keeps_prompt_memory_governance_and_exports_separate() {
    let env = IsolatedTopagent::new();
    env.write_current_workspace_state();
    fs::write(
        env.workspace.join(".topagent/USER.md"),
        "# Operator Model\n\n\
         ## concise_final_answers\n\
         **Category:** response_style\n\
         **Updated:** <t:1>\n\
         **Preference:** Keep final answers concise.\n",
    )
    .unwrap();
    fs::write(
        env.workspace.join(".topagent/MEMORY.md"),
        "# TopAgent Memory Index\n\n\
         - title: runtime architecture | file: notes/runtime.md | status: verified | tags: runtime | note: service layout\n\
         - title: approval mailbox compaction | file: procedures/1700000000-approval-mailbox.md | status: verified | tags: procedure, workflow | note: routed command handling\n",
    )
    .unwrap();
    fs::write(
        env.workspace.join(".topagent/notes/runtime.md"),
        "# Runtime Architecture\nservice layout details\n",
    )
    .unwrap();
    let procedure_path = env
        .workspace
        .join(".topagent/procedures/1700000000-approval-mailbox.md");
    write_procedure(&procedure_path, "active");
    let trajectory_path = env
        .workspace
        .join(".topagent/trajectories/trj-1700000000-approval-mailbox.json");
    write_exportable_trajectory(&trajectory_path);

    let status_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["memory", "status"])
            .assert()
            .success(),
    );
    assert!(status_output.contains("Operator model: 1 preference(s)"));
    assert!(status_output.contains("Workspace index: 2 entries"));
    assert!(status_output.contains("Notes: 1 note(s)"));
    assert!(status_output.contains("Procedures: 1 active, 0 superseded, 0 disabled"));
    assert!(status_output.contains("Trajectories: 1 local, 0 ready, 0 exported"));
    assert_current_workspace_state_layout(
        &env.workspace,
        &[
            "MEMORY.md",
            "USER.md",
            "exports",
            "notes",
            "procedures",
            "trajectories",
            "workspace-state.json",
        ],
    );

    let recall_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args([
                "memory",
                "recall",
                "repair approval mailbox compaction and runtime architecture",
            ])
            .assert()
            .success(),
    );
    assert!(recall_output.contains("Operator preferences: concise_final_answers"));
    assert!(recall_output
        .contains("Procedure files: .topagent/procedures/1700000000-approval-mailbox.md"));
    assert!(recall_output.contains("notes/runtime.md"));
    assert!(!recall_output.contains("trj-1700000000-approval-mailbox"));

    let list_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["procedure", "list"])
            .assert()
            .success(),
    );
    assert!(list_output.contains("Approval Mailbox Compaction Playbook"));
    assert!(list_output.contains("active"));

    let show_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["procedure", "show", "1700000000-approval"])
            .assert()
            .success(),
    );
    assert!(show_output.contains("## Steps"));
    assert!(show_output.contains("Patch the router seam"));

    let trajectory_list = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["memory", "trajectory", "list"])
            .assert()
            .success(),
    );
    assert!(trajectory_list.contains("local_only"));
    assert!(trajectory_list.contains("Repair approval mailbox workflow"));

    let review_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["memory", "trajectory", "review", "trj-1700000000"])
            .assert()
            .success(),
    );
    assert!(review_output.contains("Ready for export"));
    assert!(fs::read_to_string(&trajectory_path)
        .unwrap()
        .contains("ready_for_export"));

    let export_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["memory", "trajectory", "export", "trj-1700000000"])
            .assert()
            .success(),
    );
    assert!(export_output.contains("Exported: .topagent/exports/trajectories/"));
    assert!(env
        .workspace
        .join(".topagent/exports/trajectories/trj-1700000000-approval-mailbox.json")
        .is_file());

    let disable_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args([
                "procedure",
                "disable",
                "1700000000-approval",
                "--reason",
                "superseded by current router test",
            ])
            .assert()
            .success(),
    );
    assert!(
        disable_output.contains("Disabled: .topagent/procedures/1700000000-approval-mailbox.md")
    );
    assert!(fs::read_to_string(&procedure_path)
        .unwrap()
        .contains("**Status:** disabled"));

    let prune_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["procedure", "prune"])
            .assert()
            .success(),
    );
    assert!(prune_output.contains("Removed: 1"));
    assert!(!procedure_path.exists());
    assert!(
        !fs::read_to_string(env.workspace.join(".topagent/MEMORY.md"))
            .unwrap()
            .contains("procedures/1700000000-approval-mailbox.md")
    );
    assert!(env.workspace.join(".topagent/notes/runtime.md").is_file());
    assert!(env
        .workspace
        .join(".topagent/trajectories/trj-1700000000-approval-mailbox.json")
        .is_file());
}

#[test]
fn service_lifecycle_flow_controls_installed_background_service() {
    let env = IsolatedTopagent::new();
    env.write_current_workspace_state();
    env.write_managed_env(
        "OpenRouter",
        "anthropic/claude-sonnet-4.6",
        Some("openrouter-secret"),
        None,
    );
    env.write_managed_unit();
    let systemctl = env.fake_systemctl();

    let status_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .arg("status")
            .assert()
            .success(),
    );
    assert!(status_output.contains("Installation present: yes"));
    assert!(status_output.contains("Service installed: yes"));
    assert!(status_output.contains("Enabled: yes"));
    assert!(status_output.contains("Running: yes"));

    let start_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["service", "start"])
            .assert()
            .success(),
    );
    assert!(start_output.contains("TopAgent service started."));
    assert!(start_output.contains("topagent service stop"));

    let stop_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["service", "stop"])
            .assert()
            .success(),
    );
    assert!(stop_output.contains("TopAgent service stopped."));
    assert!(stop_output.contains("topagent service start"));

    let restart_output = output_stdout(
        env.command()
            .env("PATH", systemctl.path())
            .args(["service", "restart"])
            .assert()
            .success(),
    );
    assert!(restart_output.contains("TopAgent service restarted."));
    assert!(restart_output.contains("topagent status"));

    assert_current_workspace_state_layout(
        &env.workspace,
        &[
            "MEMORY.md",
            "exports",
            "notes",
            "procedures",
            "trajectories",
            "workspace-state.json",
        ],
    );

    let calls = systemctl.calls();
    assert!(calls.contains(&"--user start topagent-telegram.service".to_string()));
    assert!(calls.contains(&"--user stop topagent-telegram.service".to_string()));
    assert!(calls.contains(&"--user restart topagent-telegram.service".to_string()));
}

#[test]
fn run_snapshot_absent_flow_reports_explicit_none_without_creating_state() {
    let env = IsolatedTopagent::new();

    let status_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "status"])
            .assert()
            .success(),
    );
    assert!(status_output.contains("Run snapshot:           none"));
    assert!(status_output.contains("Telegram transcripts: 0 chat files"));

    let diff_output = output_stdout(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "diff"])
            .assert()
            .success(),
    );
    assert!(diff_output.contains("No active workspace run snapshot found."));

    let restore_stderr = output_stderr(
        env.command()
            .arg("--workspace")
            .arg(&env.workspace)
            .args(["run", "restore"])
            .assert()
            .failure(),
    );
    assert!(restore_stderr.contains("No active workspace run snapshot found."));
    assert!(!env.workspace.join(".topagent").exists());
}

#[test]
fn uninstall_flow_preserves_workspace_state_until_explicit_purge() {
    let standard = IsolatedTopagent::new();
    standard.write_current_workspace_state();
    standard.write_managed_env(
        "OpenRouter",
        "minimax/minimax-m2.7",
        Some("openrouter-secret"),
        None,
    );
    standard.write_managed_unit();
    standard.write_openrouter_cache(&["minimax/minimax-m2.7"]);
    let standard_systemctl = standard.fake_systemctl();
    fs::write(
        standard.workspace.join(".topagent/notes/operator.md"),
        "# Operator\nkeep after standard uninstall\n",
    )
    .unwrap();

    let standard_output = output_stdout(
        standard
            .command()
            .env("PATH", standard_systemctl.path())
            .arg("uninstall")
            .assert()
            .success(),
    );

    assert!(standard_output.contains("Mode: standard"));
    assert!(standard_output.contains("Stopped: yes"));
    assert!(standard_output.contains("Disabled: yes"));
    assert!(standard_output.contains("Daemon reload: yes"));
    assert!(!standard.service_env_path().exists());
    assert!(!standard.service_unit_path().exists());
    assert!(standard.workspace.is_dir());
    assert!(standard.workspace.join(".topagent").is_dir());
    assert!(standard
        .workspace
        .join(".topagent/notes/operator.md")
        .is_file());
    assert!(standard.model_cache_path().is_file());
    assert_current_workspace_state_layout(
        &standard.workspace,
        &[
            "MEMORY.md",
            "exports",
            "notes",
            "procedures",
            "trajectories",
            "workspace-state.json",
        ],
    );
    let standard_calls = standard_systemctl.calls();
    assert!(standard_calls.contains(&"--user stop topagent-telegram.service".to_string()));
    assert!(standard_calls.contains(&"--user disable topagent-telegram.service".to_string()));
    assert!(standard_calls.contains(&"--user daemon-reload".to_string()));

    let purge = IsolatedTopagent::new();
    purge.write_current_workspace_state();
    purge.write_managed_env(
        "OpenRouter",
        "minimax/minimax-m2.7",
        Some("openrouter-secret"),
        None,
    );
    purge.write_managed_unit();
    purge.write_openrouter_cache(&["minimax/minimax-m2.7"]);
    let purge_systemctl = purge.fake_systemctl();
    fs::create_dir_all(purge.workspace.join(".topagent/telegram-history")).unwrap();
    fs::write(
        purge
            .workspace
            .join(".topagent/telegram-history/chat-7.json"),
        "{}",
    )
    .unwrap();

    let purge_output = output_stdout(
        purge
            .command()
            .env("PATH", purge_systemctl.path())
            .args(["uninstall", "--purge"])
            .assert()
            .success(),
    );

    assert!(purge_output.contains("Mode: purge"));
    assert!(purge_output.contains("Stopped: yes"));
    assert!(purge_output.contains("Disabled: yes"));
    assert!(purge_output.contains("Daemon reload: yes"));
    assert!(!purge.service_env_path().exists());
    assert!(!purge.service_unit_path().exists());
    assert!(purge.workspace.is_dir());
    assert!(!purge.workspace.join(".topagent").exists());
    assert!(!purge.model_cache_path().exists());
    let purge_calls = purge_systemctl.calls();
    assert!(purge_calls.contains(&"--user stop topagent-telegram.service".to_string()));
    assert!(purge_calls.contains(&"--user disable topagent-telegram.service".to_string()));
    assert!(purge_calls.contains(&"--user daemon-reload".to_string()));
}

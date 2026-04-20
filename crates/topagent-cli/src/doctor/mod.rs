mod lint;
mod render;
mod service;
mod types;
mod workspace;

pub(crate) use types::{CheckLevel, CheckResult};

use crate::config::defaults::CliParams;
use crate::config::workspace::resolve_workspace_path;
use render::print_report;
use service::check_service_config;
use workspace::{
    check_external_tools, check_generated_tools, check_hooks_manifest, check_workspace_files,
    check_workspace_layout,
};

// Expose lint functions for the `memory lint` command used in memory_cli.
pub(crate) use lint::{lint_memory_md_content, lint_user_md_content};

pub(crate) fn run_doctor(params: CliParams) -> anyhow::Result<()> {
    let checks = run_doctor_checks(&params);
    print_report(&checks);
    let has_errors = checks.iter().any(|c| c.level == CheckLevel::Error);
    if has_errors {
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) fn run_doctor_checks(params: &CliParams) -> Vec<CheckResult> {
    let mut checks = Vec::new();

    let workspace = resolve_workspace_path(params.workspace.clone());
    match workspace {
        Ok(ws) => {
            checks.push(CheckResult {
                name: "workspace path",
                level: CheckLevel::Ok,
                detail: format!("exists: {}", ws.display()),
                hint: None,
            });
            check_workspace_layout(&ws, &mut checks);
            check_workspace_files(&ws, &mut checks);
            check_generated_tools(&ws, &mut checks);
            check_external_tools(&ws, &mut checks);
            check_hooks_manifest(&ws, &mut checks);
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "workspace path",
                level: CheckLevel::Error,
                detail: format!("unresolvable: {}", err),
                hint: Some("pass --workspace /path or run from a repo directory".to_string()),
            });
        }
    }

    check_service_config(params, &mut checks);
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MEMORY_INDEX_RELATIVE_PATH;
    use tempfile::TempDir;

    fn healthy_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();

        std::fs::create_dir_all(ws.join(".topagent/topics")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/lessons")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/procedures")).unwrap();
        std::fs::create_dir_all(ws.join(".topagent/trajectories")).unwrap();

        std::fs::write(
            ws.join(MEMORY_INDEX_RELATIVE_PATH),
            "# TopAgent Memory Index\n",
        )
        .unwrap();

        temp
    }

    #[test]
    fn test_doctor_full_healthy_workspace_has_no_errors() {
        let temp = healthy_workspace();
        std::fs::write(
            temp.path().join(".topagent/hooks.toml"),
            r#"[[hooks]]
event = "pre_tool"
command = "echo ok"
label = "test hook""#,
        )
        .unwrap();
        std::fs::write(
            temp.path().join(workspace::EXTERNAL_TOOLS_RELATIVE_PATH),
            r#"[{"name":"ls_tool","description":"list files","command":"ls","argv_template":["."],"sandbox":"workspace"}]"#,
        )
        .unwrap();

        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: None,
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let checks = run_doctor_checks(&params);
        let model_check = checks.iter().find(|c| c.name == "model config").unwrap();
        assert!(
            model_check.level == CheckLevel::Warning || model_check.level == CheckLevel::Ok,
            "model config should be warning or ok, got {:?}",
            model_check.level
        );
    }

    #[test]
    fn test_doctor_missing_topagent_dir_reports_error() {
        let temp = TempDir::new().unwrap();
        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: Some("openai/gpt-4o".to_string()),
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let checks = run_doctor_checks(&params);
        assert!(
            checks
                .iter()
                .any(|c| c.level == CheckLevel::Error && c.name == "workspace layout"),
            "missing .topagent/ should report error"
        );
    }

    #[test]
    fn test_doctor_read_only_guarantee() {
        use topagent_core::user_profile_path;

        let temp = healthy_workspace();
        let memory_path = temp.path().join(MEMORY_INDEX_RELATIVE_PATH);
        let user_path = user_profile_path(temp.path());
        let before_memory = std::fs::read_to_string(&memory_path).unwrap();
        let before_user = std::fs::read_to_string(&user_path).unwrap_or_default();
        let before_files = std::fs::read_dir(temp.path().join(".topagent"))
            .unwrap()
            .filter_map(|e| e.ok())
            .count();

        let params = CliParams {
            api_key: Some("test-key".to_string()),
            opencode_api_key: None,
            model: Some("openai/gpt-4o".to_string()),
            workspace: Some(temp.path().to_path_buf()),
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
            generated_tool_authoring: None,
        };
        let _ = run_doctor_checks(&params);

        let after_memory = std::fs::read_to_string(&memory_path).unwrap();
        let after_user = std::fs::read_to_string(&user_path).unwrap_or_default();
        let after_files = std::fs::read_dir(temp.path().join(".topagent"))
            .unwrap()
            .filter_map(|e| e.ok())
            .count();

        assert_eq!(before_memory, after_memory, "doctor mutated MEMORY.md");
        assert_eq!(before_user, after_user, "doctor mutated USER.md");
        assert_eq!(before_files, after_files, "doctor added or removed files");
    }
}

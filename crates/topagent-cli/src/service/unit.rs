use anyhow::Result;
use std::path::Path;

use crate::config::runtime::TelegramModeConfig;
use crate::managed_files::TOPAGENT_MANAGED_HEADER;
use crate::operational_paths::ServicePaths;

pub(super) fn render_service_unit_file(
    current_exe: &Path,
    config: &TelegramModeConfig,
    paths: &ServicePaths,
) -> Result<String> {
    let exec_start = render_service_exec_start(current_exe);
    let workspace = config.workspace.display().to_string();
    let env_path = paths.env_path.display().to_string();
    Ok(format!(
        "{header}
[Unit]
Description=TopAgent Telegram bot
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
WorkingDirectory={working_directory}
EnvironmentFile={env_file}
ExecStart={exec_start}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
",
        header = TOPAGENT_MANAGED_HEADER,
        working_directory = escape_systemd_value(&workspace),
        env_file = escape_systemd_value(&env_path),
        exec_start = exec_start,
    ))
}

fn render_service_exec_start(current_exe: &Path) -> String {
    let mut args = [current_exe.display().to_string(), "telegram".to_string()];
    args.iter_mut().for_each(|arg| {
        if arg.contains('\n') {
            *arg = arg.replace('\n', " ");
        }
    });
    args.iter()
        .map(|arg| escape_systemd_value(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn escape_systemd_value(value: &str) -> String {
    let safe = !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@')
        });
    if safe {
        return value.to_string();
    }

    let mut escaped = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

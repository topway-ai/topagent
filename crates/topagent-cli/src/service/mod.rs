mod install;
mod lifecycle;
pub mod managed_env;
mod model;
mod state;
mod upgrade;

use anyhow::Result;

use crate::config::CliParams;

pub(crate) use install::run_install;
pub(crate) use lifecycle::{run_status, run_uninstall};
pub(crate) use upgrade::run_upgrade;

pub(crate) fn run_service_command(
    command: crate::ServiceCommands,
    params: CliParams,
    purge: bool,
) -> Result<()> {
    match command {
        crate::ServiceCommands::Install { token } => install::run_service_install(token, params),
        crate::ServiceCommands::Status => lifecycle::run_service_status(params),
        crate::ServiceCommands::Start => lifecycle::run_service_start(),
        crate::ServiceCommands::Stop => lifecycle::run_service_stop(),
        crate::ServiceCommands::Restart => lifecycle::run_service_restart(),
        crate::ServiceCommands::Uninstall { .. } => lifecycle::run_service_uninstall(purge),
    }
}

pub(crate) fn run_model_command(command: crate::ModelCommands, params: CliParams) -> Result<()> {
    model::run_model_command(command, params)
}

/// Returns a human-readable systemd active-state label for the Telegram
/// service (e.g. "active", "inactive", "failed"), or a descriptive fallback
/// when systemd is unavailable or the probe fails. Used by `topagent run status`.
pub(crate) fn query_service_active_state() -> String {
    use crate::operational_paths::service_paths;

    let Ok(paths) = service_paths() else {
        return "unknown".to_string();
    };
    let probe = state::load_service_probe(&paths);
    if let Some(snapshot) = &probe.snapshot {
        snapshot
            .active_state
            .as_deref()
            .unwrap_or("unknown")
            .to_string()
    } else if let Some(err) = &probe.systemd_error {
        format!("unavailable ({})", err)
    } else {
        "unknown".to_string()
    }
}

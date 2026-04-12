mod install;
mod lifecycle;
mod managed_env;
mod model;
mod state;

use anyhow::Result;

use crate::config::CliParams;

pub(crate) use install::run_install;
pub(crate) use lifecycle::{run_status, run_uninstall};

pub(crate) fn run_service_command(
    command: crate::ServiceCommands,
    params: CliParams,
) -> Result<()> {
    match command {
        crate::ServiceCommands::Install { token } => install::run_service_install(token, params),
        crate::ServiceCommands::Status => lifecycle::run_service_status(params),
        crate::ServiceCommands::Start => lifecycle::run_service_start(),
        crate::ServiceCommands::Stop => lifecycle::run_service_stop(),
        crate::ServiceCommands::Restart => lifecycle::run_service_restart(),
        crate::ServiceCommands::Uninstall => lifecycle::run_service_uninstall(),
    }
}

pub(crate) fn run_model_command(command: crate::ModelCommands, params: CliParams) -> Result<()> {
    model::run_model_command(command, params)
}

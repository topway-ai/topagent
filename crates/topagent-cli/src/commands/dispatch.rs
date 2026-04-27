use anyhow::Result;

use crate::access::run_access_command;
use crate::commands::memory_cli::run_memory_command;
use crate::commands::procedure_cli::run_procedure_command;
use crate::config::defaults::CliParams;
use crate::doctor::run_doctor;
use crate::service::{
    run_install, run_model_command, run_service_command, run_status, run_uninstall, run_upgrade,
};
use crate::telegram::run_telegram;

use super::config::run_config_inspect;
use super::oneshot::run_one_shot;
use super::run::{run_session_status, run_snapshot_diff, run_snapshot_restore};
use super::types::{Commands, ConfigCommands, RunCommands};

pub(crate) fn dispatch(
    command: Option<Commands>,
    instruction: Option<String>,
    params: CliParams,
) -> Result<()> {
    match command {
        Some(Commands::Install) => run_install(params),
        Some(Commands::Status) => run_status(params),
        Some(Commands::Doctor) => run_doctor(params),
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Inspect => run_config_inspect(params),
        },
        Some(Commands::Model { command }) => run_model_command(command, params),
        Some(Commands::Memory { command }) => run_memory_command(command, params.workspace),
        Some(Commands::Procedure { command }) => run_procedure_command(command, params.workspace),
        Some(Commands::Run { command }) => match command {
            RunCommands::Status => run_session_status(params.workspace),
            RunCommands::Diff => run_snapshot_diff(params.workspace),
            RunCommands::Restore => run_snapshot_restore(params.workspace),
        },
        Some(Commands::Access { command }) => run_access_command(command),
        Some(Commands::Upgrade { use_cargo }) => run_upgrade(use_cargo),
        Some(Commands::Uninstall { purge }) => run_uninstall(purge),
        Some(Commands::Service { command }) => run_service_command(command),
        Some(Commands::Telegram { token }) => run_telegram(token, params),
        None => {
            let instruction = instruction.ok_or_else(|| {
                anyhow::anyhow!("Instruction required. Run: topagent \"summarize this repository\"")
            })?;
            run_one_shot(params, instruction)
        }
    }
}

pub(crate) fn cli_to_params(
    cli: super::types::Cli,
) -> (Option<Commands>, Option<String>, CliParams) {
    let params = CliParams {
        api_key: cli.api_key,
        opencode_api_key: cli.opencode_api_key,
        model: cli.model,
        workspace: cli.workspace,
        max_steps: cli.max_steps,
        max_retries: cli.max_retries,
        timeout_secs: cli.timeout_secs,
    };
    (cli.command, cli.instruction, params)
}

// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod commands;
mod config;
mod doctor;
mod managed_files;
mod memory;
mod openrouter_models;
mod operational_paths;
mod progress;
mod run_setup;
mod service;
mod telegram;
mod workspace_state;

use anyhow::Result;
use clap::Parser;

use crate::commands::{cli_to_params, dispatch, Cli};

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let (command, instruction, params) = cli_to_params(cli);
    dispatch(command, instruction, params)
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

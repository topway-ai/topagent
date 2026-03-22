use anyhow::Result;
use clap::Parser;
use pi_core::{
    context::ExecutionContext, tools::default_tools, Agent, Message, OpenRouterProvider, Provider,
    ProviderResponse, RuntimeOptions,
};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(author, version, about = "pi-coding-agent: minimal coding agent")]
struct Cli {
    #[arg(long, help = "OpenRouter API key")]
    api_key: Option<String>,

    #[arg(
        long,
        default_value = "openrouter/anthropic/claude-3.5-haiku",
        help = "Model to use"
    )]
    model: String,

    #[arg(long, help = "Working directory for file operations")]
    cwd: Option<PathBuf>,

    #[arg(long, help = "Maximum steps for agent loop")]
    max_steps: Option<usize>,

    #[arg(long, help = "Maximum provider retries")]
    max_retries: Option<usize>,

    #[arg(help = "Instruction for the agent")]
    instruction: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Cli::parse();
    let workspace = args.cwd.unwrap_or_else(|| std::env::current_dir().unwrap());
    let ctx = ExecutionContext::new(workspace);

    info!("starting agent with instruction: {}", args.instruction);
    info!("workspace root: {:?}", ctx.workspace_root);

    let options = RuntimeOptions::new()
        .with_max_steps(args.max_steps.unwrap_or(50))
        .with_max_provider_retries(args.max_retries.unwrap_or(3));

    let provider: Box<dyn Provider> = if let Some(api_key) = args.api_key {
        Box::new(OpenRouterProvider::with_timeout(
            api_key,
            args.model,
            options.provider_timeout_secs,
        ))
    } else {
        info!("No API key provided, using echo provider (for testing)");
        Box::new(EchoProvider)
    };

    let mut agent = Agent::with_options(provider, default_tools().into_inner(), options);

    match agent.run(&ctx, &args.instruction) {
        Ok(result) => {
            println!("{}", result);
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

struct EchoProvider;

impl Provider for EchoProvider {
    fn complete(&self, messages: &[pi_core::Message]) -> Result<ProviderResponse, pi_core::Error> {
        let last = messages
            .last()
            .map(|m| m.as_text().unwrap_or(""))
            .unwrap_or("");
        if last.contains("read") {
            Ok(ProviderResponse::Message(Message::assistant(
                "I would read that file for you.",
            )))
        } else if last.contains("write") || last.contains("edit") {
            Ok(ProviderResponse::Message(Message::assistant(
                "I would write that file for you.",
            )))
        } else if last.contains("bash") || last.contains("run") || last.contains("execute") {
            Ok(ProviderResponse::Message(Message::assistant(
                "I would execute that command for you.",
            )))
        } else {
            Ok(ProviderResponse::Message(Message::assistant(
                "Understood. I can help with file operations, bash commands, and code editing.",
            )))
        }
    }
}

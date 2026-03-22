use anyhow::Result;
use clap::Parser;
use pi_core::{
    context::ExecutionContext,
    tools::{make_tools, Tool},
    Agent, Message, OpenRouterProvider, Provider, ProviderResponse,
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

    let tools: Vec<Box<dyn Tool>> = make_tools(&ctx).into_values().collect();

    let provider: Box<dyn Provider> = if let Some(api_key) = args.api_key {
        Box::new(OpenRouterProvider::new(api_key, args.model))
    } else {
        info!("No API key provided, using echo provider (for testing)");
        Box::new(EchoProvider)
    };

    let mut agent = Agent::new(provider, tools);

    let result = agent.run(&ctx, &args.instruction)?;
    println!("{}", result);

    Ok(())
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

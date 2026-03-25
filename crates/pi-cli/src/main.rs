use anyhow::Result;
use clap::{Parser, Subcommand};
use pi_core::{
    channel::{ChannelAdapter, OutgoingMessage},
    context::ExecutionContext,
    create_provider,
    model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory},
    tools::default_tools,
    Agent, Message, Provider, ProviderResponse, RuntimeOptions, TelegramAdapter,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(author, version, about = "pi-coding-agent: minimal coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        #[arg(long, help = "OpenRouter API key")]
        api_key: Option<String>,

        #[arg(long, default_value = "openrouter", help = "Provider to use")]
        provider: String,

        #[arg(long, help = "Model to use (overrides default for provider)")]
        model: Option<String>,

        #[arg(long, help = "Working directory for file operations")]
        cwd: Option<PathBuf>,

        #[arg(long, help = "Maximum steps for agent loop")]
        max_steps: Option<usize>,

        #[arg(long, help = "Maximum provider retries")]
        max_retries: Option<usize>,

        #[arg(long, help = "Provider timeout in seconds")]
        timeout_secs: Option<u64>,

        #[arg(help = "Instruction for the agent")]
        instruction: String,
    },
    Telegram {
        #[command(subcommand)]
        telegram_command: TelegramCommands,
    },
}

#[derive(Subcommand)]
enum TelegramCommands {
    Serve {
        #[arg(long, help = "Telegram bot token")]
        token: Option<String>,

        #[arg(long, help = "Working directory for file operations")]
        cwd: Option<PathBuf>,

        #[arg(long, help = "Maximum steps for agent loop")]
        max_steps: Option<usize>,

        #[arg(long, help = "Maximum provider retries")]
        max_retries: Option<usize>,

        #[arg(long, help = "Provider timeout in seconds")]
        timeout_secs: Option<u64>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            api_key,
            provider,
            model,
            cwd,
            max_steps,
            max_retries,
            timeout_secs,
            instruction,
        } => run_one_shot(
            api_key,
            provider,
            model,
            cwd,
            max_steps,
            max_retries,
            timeout_secs,
            instruction,
        ),
        Commands::Telegram { telegram_command } => match telegram_command {
            TelegramCommands::Serve {
                token,
                cwd,
                max_steps,
                max_retries,
                timeout_secs,
            } => run_telegram_serve(token, cwd, max_steps, max_retries, timeout_secs),
        },
    }
}

fn run_one_shot(
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    cwd: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
    instruction: String,
) -> Result<()> {
    let workspace = cwd.unwrap_or_else(|| std::env::current_dir().unwrap());
    let ctx = ExecutionContext::new(workspace);

    info!("starting agent with instruction: {}", instruction);
    info!("workspace root: {:?}", ctx.workspace_root);

    let options = RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(3))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120));

    let provider_id = ProviderId::parse(&provider).map_err(|e| anyhow::anyhow!("{}", e))?;
    let route = RoutingPolicy::select_route(TaskCategory::Default, model.as_deref());

    let api_key = api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
    let provider: Box<dyn Provider> = if let Some(api_key) = api_key {
        let mut route = route;
        route.provider_id = provider_id;
        create_provider(
            &route,
            &api_key,
            default_tools().specs(),
            options.provider_timeout_secs,
        )?
    } else {
        info!("No API key provided, using echo provider (for testing)");
        Box::new(EchoProvider)
    };

    let mut agent = Agent::with_options(provider, default_tools().into_inner(), options);

    match agent.run(&ctx, &instruction) {
        Ok(result) => {
            println!("{}", result);
            Ok(())
        }
        Err(e) => {
            error!("agent error: {}", e);
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_telegram_serve(
    token: Option<String>,
    cwd: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let token = token
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .ok_or_else(|| {
            anyhow::anyhow!("Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN")
        })?;

    let workspace = cwd.unwrap_or_else(|| std::env::current_dir().unwrap());
    let ctx = ExecutionContext::new(workspace);

    let options = RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(3))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120));

    let api_key = std::env::var("OPENROUTER_API_KEY").ok().ok_or_else(|| {
        anyhow::anyhow!("OPENROUTER_API_KEY environment variable required for Telegram mode")
    })?;

    let route = RoutingPolicy::select_route(TaskCategory::Default, None);

    let adapter = TelegramAdapter::new(&token);

    if let Ok(true) = adapter.check_webhook() {
        return Err(anyhow::anyhow!(
            "Telegram webhook is configured. Please remove the webhook to use long polling mode.\n\
             Use deleteWebhook to disable the webhook: https://core.telegram.org/bots/api#deletewebhook"
        ));
    }

    let bot_info = adapter.get_me().map_err(|e| {
        anyhow::anyhow!(
            "Failed to validate bot token (getMe failed): {}. \
             Make sure TELEGRAM_BOT_TOKEN is correct.",
            e
        )
    })?;

    info!(
        "bot: @{} (id: {}) | workspace: {:?} | mode: private text chats only",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
        ctx.workspace_root
    );

    let mut session_manager = ChatSessionManager::new(route, api_key, options);
    let offset = Arc::new(Mutex::new(0i64));

    info!(" Telegram bot is running, waiting for messages...");

    loop {
        let current_offset = { *offset.lock().unwrap() };
        match adapter.get_updates(Some(current_offset), Some(30), Some(&["message"])) {
            Ok(updates) => {
                for update in updates {
                    let Some(msg) = &update.message else { continue };
                    if msg.chat.chat_type != "private" {
                        continue;
                    }

                    let chat_id = msg.chat.id;
                    let message_id = msg.message_id;
                    *offset.lock().unwrap() = update.update_id + 1;

                    let Some(text) = msg.text.clone() else {
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: "This bot currently supports text messages only.".to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    };

                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }

                    info!("received from chat {}: {}", chat_id, text);

                    if text == "/start" || text == "/help" {
                        let reply = if text == "/start" {
                            " Rust PI Coding Agent\n\n\
                             This bot responds to text messages about your project.\n\n\
                             Commands:\n\
                             /help - show this message\n\
                             /reset - clear conversation history\n\n\
                             Note: private chats only, text messages only."
                        } else {
                            " Rust PI Coding Agent\n\n\
                             Send any text message about your project.\n\
                             /reset clears your conversation history."
                        };
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: reply.to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    }

                    if text == "/reset" {
                        session_manager.reset_chat(chat_id);
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: "Conversation history cleared.".to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    }

                    let response = session_manager.process_message(&ctx, chat_id, text);

                    for chunk in response {
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: chunk,
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                    }

                    let _ = adapter.acknowledge(chat_id, message_id);
                }
            }
            Err(e) => {
                error!("failed to get updates: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
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

struct ChatSessionManager {
    route: ModelRoute,
    api_key: String,
    options: RuntimeOptions,
    sessions: HashMap<i64, SessionState>,
}

struct SessionState {
    agent: Agent,
}

impl ChatSessionManager {
    fn new(route: ModelRoute, api_key: String, options: RuntimeOptions) -> Self {
        Self {
            route,
            api_key,
            options,
            sessions: HashMap::new(),
        }
    }

    fn get_or_create_session(&mut self, chat_id: i64) -> &mut Agent {
        if !self.sessions.contains_key(&chat_id) {
            let provider = create_provider(
                &self.route,
                &self.api_key,
                default_tools().specs(),
                self.options.provider_timeout_secs,
            )
            .expect("failed to create provider");
            let tools = default_tools();
            let agent = Agent::with_options(provider, tools.into_inner(), self.options.clone());
            self.sessions.insert(chat_id, SessionState { agent });
        }
        &mut self.sessions.get_mut(&chat_id).unwrap().agent
    }

    fn reset_chat(&mut self, chat_id: i64) {
        self.sessions.remove(&chat_id);
    }

    fn process_message(&mut self, ctx: &ExecutionContext, chat_id: i64, text: &str) -> Vec<String> {
        let agent = self.get_or_create_session(chat_id);

        match agent.run(ctx, text) {
            Ok(response) => {
                let max_len = 4000;
                if response.len() <= max_len {
                    vec![response]
                } else {
                    pi_core::channel::telegram::chunk_text(&response, max_len)
                }
            }
            Err(e) => {
                vec![format!("Error: {}", e)]
            }
        }
    }
}

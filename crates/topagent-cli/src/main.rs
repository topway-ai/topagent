use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use topagent_core::{
    channel::{ChannelAdapter, OutgoingMessage},
    context::ExecutionContext,
    create_provider,
    model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory},
    tools::default_tools,
    Agent, RuntimeOptions, TelegramAdapter,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "topagent: minimal coding agent",
    arg_required_else_help = true
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "OpenRouter API key (or OPENROUTER_API_KEY)"
    )]
    api_key: Option<String>,

    #[arg(
        long,
        global = true,
        default_value = "openrouter",
        help = "Provider to use"
    )]
    provider: String,

    #[arg(
        long,
        global = true,
        help = "Model to use (overrides the default model)"
    )]
    model: Option<String>,

    #[arg(
        long = "workspace",
        alias = "cwd",
        global = true,
        help = "Workspace/repo directory (or TOPAGENT_WORKSPACE)"
    )]
    workspace: Option<PathBuf>,

    #[arg(long, global = true, help = "Maximum steps for the agent loop")]
    max_steps: Option<usize>,

    #[arg(long, global = true, help = "Maximum provider retries")]
    max_retries: Option<usize>,

    #[arg(long, global = true, help = "Provider timeout in seconds")]
    timeout_secs: Option<u64>,

    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(help = "Instruction for one-shot mode")]
    instruction: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    Telegram {
        #[arg(long, help = "Telegram bot token (or TELEGRAM_BOT_TOKEN)")]
        token: Option<String>,
    },
    #[command(hide = true)]
    Run { instruction: String },
}

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    let Cli {
        api_key,
        provider,
        model,
        workspace,
        max_steps,
        max_retries,
        timeout_secs,
        command,
        instruction,
    } = cli;

    match command {
        Some(Commands::Telegram { token }) => run_telegram(
            token,
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
        ),
        Some(Commands::Run { instruction }) => run_one_shot(
            api_key,
            provider,
            model,
            workspace,
            max_steps,
            max_retries,
            timeout_secs,
            instruction,
        ),
        None => {
            let instruction = instruction.ok_or_else(|| {
                anyhow::anyhow!("Instruction required. Run: topagent \"summarize this repository\"")
            })?;
            run_one_shot(
                api_key,
                provider,
                model,
                workspace,
                max_steps,
                max_retries,
                timeout_secs,
                instruction,
            )
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn build_runtime_options(
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> RuntimeOptions {
    RuntimeOptions::new()
        .with_max_steps(max_steps.unwrap_or(50))
        .with_max_provider_retries(max_retries.unwrap_or(3))
        .with_provider_timeout_secs(timeout_secs.unwrap_or(120))
}

fn resolve_workspace_path(workspace: Option<PathBuf>) -> Result<PathBuf> {
    let workspace = match workspace.or_else(|| std::env::var("TOPAGENT_WORKSPACE").ok().map(PathBuf::from)) {
        Some(path) => path,
        None => std::env::current_dir()
            .context(
                "Failed to determine the current directory. Use --workspace /path/to/repo or set TOPAGENT_WORKSPACE.",
            )?,
    };

    if !workspace.exists() {
        return Err(anyhow::anyhow!(
            "Workspace path does not exist: {}. Use --workspace /path/to/repo or set TOPAGENT_WORKSPACE.",
            workspace.display()
        ));
    }

    if !workspace.is_dir() {
        return Err(anyhow::anyhow!(
            "Workspace path is not a directory: {}",
            workspace.display()
        ));
    }

    workspace.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Workspace path is not accessible: {} ({})",
            workspace.display(),
            e
        )
    })
}

fn require_openrouter_api_key(api_key: Option<String>) -> Result<String> {
    let api_key = api_key
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if api_key.is_empty() {
        return Err(anyhow::anyhow!(
            "OpenRouter API key required: set --api-key or OPENROUTER_API_KEY"
        ));
    }

    Ok(api_key)
}

fn require_telegram_token(token: Option<String>) -> Result<String> {
    let token = token
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN"
        ));
    }

    if !token.contains(':') {
        return Err(anyhow::anyhow!(
            "Telegram bot token looks invalid. Expected something like 123456:ABCdef..."
        ));
    }

    Ok(token)
}

fn run_one_shot(
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
    instruction: String,
) -> Result<()> {
    let workspace = resolve_workspace_path(workspace)?;
    let ctx = ExecutionContext::new(workspace);
    let options = build_runtime_options(max_steps, max_retries, timeout_secs);
    let route = build_route(provider, model)?;
    let api_key = require_openrouter_api_key(api_key)?;

    info!(
        "starting one-shot run | provider: {} | model: {} | workspace: {}",
        route.provider_id,
        route.model_id,
        ctx.workspace_root.display()
    );
    info!("instruction: {}", instruction);

    let provider = create_provider(
        &route,
        &api_key,
        default_tools().specs(),
        options.provider_timeout_secs,
    )?;

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

fn build_route(provider: String, model: Option<String>) -> Result<ModelRoute> {
    let provider_id = ProviderId::parse(&provider).map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut route = RoutingPolicy::select_route(TaskCategory::Default, model.as_deref());
    route.provider_id = provider_id;
    Ok(route)
}

fn run_telegram(
    token: Option<String>,
    api_key: Option<String>,
    provider: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
    max_steps: Option<usize>,
    max_retries: Option<usize>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let token = require_telegram_token(token)?;
    let workspace = resolve_workspace_path(workspace)?;
    let ctx = ExecutionContext::new(workspace);
    let workspace_label = ctx.workspace_root.display().to_string();
    let options = build_runtime_options(max_steps, max_retries, timeout_secs);
    let api_key = require_openrouter_api_key(api_key)?;
    let route = build_route(provider, model)?;
    let adapter = TelegramAdapter::new(&token);

    match adapter.check_webhook() {
        Ok(true) => {
            return Err(anyhow::anyhow!(
                "Telegram webhook is configured. Please remove it before using long polling.\n\
                 Use deleteWebhook to disable the webhook: https://core.telegram.org/bots/api#deletewebhook"
            ));
        }
        Ok(false) => {}
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to check Telegram webhook state: {}. Check the bot token and network access.",
                e
            ));
        }
    }

    let bot_info = adapter.get_me().map_err(|e| {
        anyhow::anyhow!(
            "Failed to validate bot token (getMe failed): {}. \
             Make sure TELEGRAM_BOT_TOKEN is correct.",
            e
        )
    })?;

    info!(
        "starting Telegram mode | provider: {} | model: {} | workspace: {}",
        route.provider_id, route.model_id, workspace_label
    );
    info!(
        "bot: @{} (id: {}) | private text chats only | send /start in a private chat",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
    );

    let mut session_manager = ChatSessionManager::new(route, api_key, options);
    let offset = Arc::new(Mutex::new(0i64));

    info!("telegram polling started");

    loop {
        let current_offset = { *offset.lock().unwrap() };
        match adapter.get_updates(Some(current_offset), Some(30), Some(&["message"])) {
            Ok(updates) => {
                for update in updates {
                    let Some(msg) = &update.message else { continue };
                    *offset.lock().unwrap() = update.update_id + 1;
                    let chat_id = msg.chat.id;
                    let message_id = msg.message_id;

                    if msg.chat.chat_type != "private" {
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: "This bot currently supports private chats only. Open a private chat with the bot and try again.".to_string(),
                        };
                        if let Err(e) = adapter.send_message(outgoing) {
                            error!("failed to send message: {}", e);
                        }
                        continue;
                    }

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
                            format!(
                                "TopAgent\n\n\
                                 Workspace: {}\n\
                                 Mode: private text chats only\n\n\
                                 Commands:\n\
                                 /help - show this message\n\
                                 /reset - clear conversation history\n\n\
                                 Try this first message:\n\
                                 Summarize this repository and tell me the main entry points.",
                                workspace_label
                            )
                        } else {
                            format!(
                                "TopAgent\n\n\
                                 Workspace: {}\n\
                                 Send a plain text task about this workspace.\n\
                                 /reset clears your conversation history.",
                                workspace_label
                            )
                        };
                        let outgoing = OutgoingMessage {
                            chat_id,
                            text: reply,
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
                error!(
                    "failed to get Telegram updates: {}. Retrying in 5 seconds.",
                    e
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
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
                    topagent_core::channel::telegram::chunk_text(&response, max_len)
                }
            }
            Err(e) => {
                vec![format!("Error: {}", e)]
            }
        }
    }
}

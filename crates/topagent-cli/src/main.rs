// TopAgent CLI entry point - supports one-shot execution and Telegram bot mode.
// Run: topagent "task" or topagent telegram
mod progress;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::Duration;
use topagent_core::{
    channel::{ChannelAdapter, OutgoingMessage},
    context::ExecutionContext,
    create_provider,
    model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory},
    tools::default_tools,
    Agent, CancellationToken, ProgressCallback, ProgressUpdate, RuntimeOptions, TelegramAdapter,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::progress::LiveProgress;

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
        help = "Workspace/repo directory override"
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
    resolve_workspace_path_with_current_dir(workspace, std::env::current_dir())
}

fn resolve_workspace_path_with_current_dir(
    workspace: Option<PathBuf>,
    current_dir: std::io::Result<PathBuf>,
) -> Result<PathBuf> {
    let workspace = match workspace {
        Some(path) => path,
        None => current_dir.context(
            "Failed to determine the current directory. Run TopAgent from your repo or pass --workspace /path/to/repo.",
        )?,
    };

    if !workspace.exists() {
        return Err(anyhow::anyhow!(
            "Workspace path does not exist: {}. Run TopAgent from a repo directory or pass --workspace /path/to/repo.",
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

fn install_ctrlc_handler(
    cancel_token: CancellationToken,
    progress_callback: ProgressCallback,
) -> Result<()> {
    let interrupt_count = Arc::new(AtomicUsize::new(0));
    ctrlc::set_handler(move || {
        let count = interrupt_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            cancel_token.cancel();
            progress_callback(ProgressUpdate::stopping());
        } else {
            eprintln!("status: forcing exit");
            std::process::exit(130);
        }
    })
    .context("Failed to install Ctrl-C handler")
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
    let cancel_token = CancellationToken::new();
    let ctx = ExecutionContext::new(workspace).with_cancel_token(cancel_token.clone());
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

    let heartbeat_interval = Duration::from_secs(options.progress_heartbeat_secs);
    let mut agent = Agent::with_options(provider, default_tools().into_inner(), options);
    let progress = LiveProgress::for_cli(heartbeat_interval);
    let progress_callback = progress.callback();
    install_ctrlc_handler(cancel_token, progress_callback.clone())?;
    agent.set_progress_callback(Some(progress_callback));
    let result = agent.run(&ctx, &instruction);
    agent.set_progress_callback(None);
    progress.wait();

    match result {
        Ok(result) => {
            println!("{}", result);
            Ok(())
        }
        Err(topagent_core::Error::Stopped(_)) => {
            info!("one-shot run stopped by user");
            std::process::exit(130);
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
    let mut polling_retries = 0usize;

    info!("telegram polling started");

    loop {
        session_manager.collect_finished_tasks();
        let current_offset = { *offset.lock().unwrap() };
        match adapter.get_updates(Some(current_offset), Some(30), Some(&["message"])) {
            Ok(updates) => {
                if polling_retries > 0 {
                    session_manager.notify_polling_recovered();
                }
                polling_retries = 0;
                session_manager.collect_finished_tasks();
                for update in updates {
                    session_manager.collect_finished_tasks();
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
                                 /stop - stop the current task\n\
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
                                 /stop requests cancellation of the current task.\n\
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

                    if text == "/stop" {
                        let reply = if session_manager.stop_chat(chat_id) {
                            "Stopping current task...".to_string()
                        } else {
                            "No task is currently running.".to_string()
                        };
                        send_telegram_chunks(&adapter, chat_id, vec![reply]);
                        continue;
                    }

                    if text == "/reset" {
                        let reply = if session_manager.is_task_running(chat_id) {
                            "A task is still running. Send /stop and wait for it to finish before /reset."
                                .to_string()
                        } else {
                            session_manager.reset_chat(chat_id);
                            "Conversation history cleared.".to_string()
                        };
                        send_telegram_chunks(&adapter, chat_id, vec![reply]);
                        continue;
                    }

                    let response = session_manager.start_message(&ctx, &adapter, chat_id, text);
                    send_telegram_chunks(&adapter, chat_id, response);
                    let _ = adapter.acknowledge(chat_id, message_id);
                }
            }
            Err(e) => {
                polling_retries += 1;
                session_manager.notify_polling_retry();
                error!(
                    "failed to get Telegram updates: {}. Retrying in 5 seconds (attempt {}).",
                    e, polling_retries
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }
}

fn send_telegram_chunks(adapter: &TelegramAdapter, chat_id: i64, chunks: Vec<String>) {
    for chunk in chunks {
        let outgoing = OutgoingMessage {
            chat_id,
            text: chunk,
        };
        if let Err(e) = adapter.send_message(outgoing) {
            error!("failed to send message: {}", e);
        }
    }
}

struct ChatSessionManager {
    route: ModelRoute,
    api_key: String,
    options: RuntimeOptions,
    sessions: HashMap<i64, SessionState>,
    completed_tx: mpsc::Sender<CompletedChatTask>,
    completed_rx: mpsc::Receiver<CompletedChatTask>,
}

enum SessionState {
    Idle(Agent),
    Running(RunningChatTask),
}

struct RunningChatTask {
    cancel_token: CancellationToken,
    progress_callback: Option<ProgressCallback>,
}

struct CompletedChatTask {
    chat_id: i64,
    agent: Agent,
}

impl ChatSessionManager {
    fn new(route: ModelRoute, api_key: String, options: RuntimeOptions) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        Self {
            route,
            api_key,
            options,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
        }
    }

    fn create_agent(&self) -> Agent {
        let provider = create_provider(
            &self.route,
            &self.api_key,
            default_tools().specs(),
            self.options.provider_timeout_secs,
        )
        .expect("failed to create provider");
        let tools = default_tools();
        Agent::with_options(provider, tools.into_inner(), self.options.clone())
    }

    fn collect_finished_tasks(&mut self) {
        while let Ok(task) = self.completed_rx.try_recv() {
            self.sessions
                .insert(task.chat_id, SessionState::Idle(task.agent));
        }
    }

    fn is_task_running(&self, chat_id: i64) -> bool {
        matches!(self.sessions.get(&chat_id), Some(SessionState::Running(_)))
    }

    fn stop_chat(&mut self, chat_id: i64) -> bool {
        let Some(SessionState::Running(task)) = self.sessions.get(&chat_id) else {
            return false;
        };

        task.cancel_token.cancel();
        if let Some(callback) = &task.progress_callback {
            callback(ProgressUpdate::stopping());
        }
        true
    }

    fn notify_polling_retry(&self) {
        self.broadcast_progress(ProgressUpdate::retrying(
            "Telegram polling failed, retrying connection...",
        ));
    }

    fn notify_polling_recovered(&self) {
        self.broadcast_progress(ProgressUpdate::working(
            "Telegram connection restored. Task still running...",
        ));
    }

    fn broadcast_progress(&self, update: ProgressUpdate) {
        for session in self.sessions.values() {
            let SessionState::Running(task) = session else {
                continue;
            };

            if let Some(callback) = &task.progress_callback {
                callback(update.clone());
            }
        }
    }

    fn reset_chat(&mut self, chat_id: i64) {
        self.sessions.remove(&chat_id);
    }

    fn start_message(
        &mut self,
        ctx: &ExecutionContext,
        adapter: &TelegramAdapter,
        chat_id: i64,
        text: &str,
    ) -> Vec<String> {
        self.collect_finished_tasks();
        if self.is_task_running(chat_id) {
            return vec![
                "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                    .to_string(),
            ];
        }

        let heartbeat_interval = Duration::from_secs(self.options.progress_heartbeat_secs);
        let mut agent = match self.sessions.remove(&chat_id) {
            Some(SessionState::Idle(agent)) => agent,
            Some(SessionState::Running(task)) => {
                self.sessions.insert(chat_id, SessionState::Running(task));
                return vec![
                    "A task is already running in this chat. Send /stop to cancel it or wait for it to finish."
                        .to_string(),
                ];
            }
            None => self.create_agent(),
        };

        let cancel_token = CancellationToken::new();
        let run_ctx = ctx.clone().with_cancel_token(cancel_token.clone());
        let progress =
            match LiveProgress::for_telegram(heartbeat_interval, adapter.clone(), chat_id) {
                Ok(progress) => Some(progress),
                Err(err) => {
                    error!("failed to start Telegram live progress: {}", err);
                    None
                }
            };
        let progress_callback = progress.as_ref().map(|progress| progress.callback());
        let worker_progress_callback = progress_callback.clone();
        let completed_tx = self.completed_tx.clone();
        let adapter = adapter.clone();
        let instruction = text.to_string();

        thread::spawn(move || {
            if let Some(callback) = &worker_progress_callback {
                agent.set_progress_callback(Some(callback.clone()));
            }

            let result = agent.run(&run_ctx, &instruction);
            agent.set_progress_callback(None);

            if let Some(progress) = progress {
                progress.wait();
            }

            match result {
                Ok(response) => {
                    let max_len = 4000;
                    let chunks = if response.len() <= max_len {
                        vec![response]
                    } else {
                        topagent_core::channel::telegram::chunk_text(&response, max_len)
                    };
                    send_telegram_chunks(&adapter, chat_id, chunks);
                }
                Err(topagent_core::Error::Stopped(_)) => {}
                Err(e) => {
                    send_telegram_chunks(&adapter, chat_id, vec![format!("Error: {}", e)]);
                }
            }

            let _ = completed_tx.send(CompletedChatTask { chat_id, agent });
        });

        self.sessions.insert(
            chat_id,
            SessionState::Running(RunningChatTask {
                cancel_token,
                progress_callback,
            }),
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_workspace_path_with_current_dir, ChatSessionManager, RunningChatTask, SessionState,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::{
        CancellationToken, ModelRoute, ProgressKind, ProgressUpdate, RuntimeOptions,
    };

    #[test]
    fn test_workspace_defaults_to_current_directory_for_one_shot_and_telegram() {
        let temp = TempDir::new().unwrap();
        let resolved =
            resolve_workspace_path_with_current_dir(None, Ok(temp.path().to_path_buf())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_override_beats_current_directory_for_one_shot_and_telegram() {
        let current = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(override_dir.path().to_path_buf()),
            Ok(current.path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_workspace_resolution_fails_when_current_directory_is_unavailable() {
        let err = resolve_workspace_path_with_current_dir(
            None,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Failed to determine the current directory"));
    }

    #[test]
    fn test_workspace_override_ignores_invalid_current_directory() {
        let override_dir = TempDir::new().unwrap();
        let resolved = resolve_workspace_path_with_current_dir(
            Some(PathBuf::from(override_dir.path())),
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "current directory missing",
            )),
        )
        .unwrap();
        assert_eq!(resolved, override_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_stop_chat_cancels_running_task_and_emits_stopping_progress() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let cancel_token = CancellationToken::new();
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: Some(progress_callback),
            }),
        );

        assert!(manager.stop_chat(42));
        assert!(cancel_token.is_cancelled());
        assert!(updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update == &ProgressUpdate::stopping()));
    }

    #[test]
    fn test_stop_chat_returns_false_when_idle() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        assert!(!manager.stop_chat(42));
    }

    #[test]
    fn test_notify_polling_retry_emits_retrying_progress_to_running_chat() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
        );

        manager.notify_polling_retry();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Retrying
                && update
                    .message
                    .contains("Telegram polling failed, retrying connection")
        }));
    }

    #[test]
    fn test_notify_polling_recovered_emits_working_progress_to_running_chat() {
        let route = ModelRoute::openrouter("test-model");
        let mut manager =
            ChatSessionManager::new(route, "test-key".to_string(), RuntimeOptions::default());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            SessionState::Running(RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
            }),
        );

        manager.notify_polling_recovered();

        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| {
            update.kind == ProgressKind::Working
                && update
                    .message
                    .contains("Telegram connection restored. Task still running")
        }));
    }
}

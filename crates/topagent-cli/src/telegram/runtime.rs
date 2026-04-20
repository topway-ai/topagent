use anyhow::Result;
use topagent_core::{context::ExecutionContext, TelegramAdapter, POLL_TIMEOUT_SECS};
use tracing::{debug, error, info, warn};

use crate::config::defaults::{CliParams, load_persisted_telegram_defaults};
use crate::config::runtime::resolve_telegram_mode_config;
use crate::operational_paths::managed_service_env_path;
use crate::telegram::router::{route_callback_query, route_message};
use crate::telegram::session::ChatSessionManager;

pub(crate) fn run_telegram(token: Option<String>, params: CliParams) -> Result<()> {
    let persisted_defaults = load_persisted_telegram_defaults().unwrap_or_default();
    let config = resolve_telegram_mode_config(token, params, persisted_defaults)?;
    let api_key = config.effective_api_key()?;
    let token = config.token;
    let workspace = config.workspace;
    let configured_default_model = config.configured_default_model;
    let allowed_dm_username = config.allowed_dm_username;
    let bound_dm_user_id = config.bound_dm_user_id;
    let mut secrets = topagent_core::SecretRegistry::new();
    if let Some(ref openrouter_key) = config.openrouter_api_key {
        secrets.register(openrouter_key);
    }
    if let Some(ref opencode_key) = config.opencode_api_key {
        secrets.register(opencode_key);
    }
    secrets.register(&token);
    let ctx = ExecutionContext::new(workspace).with_secrets(secrets.clone());
    let workspace_label = ctx.workspace_root.display().to_string();
    let options = config.options;
    let route = config.route;
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
        "starting Telegram mode | model: {} | workspace: {}",
        route.model_id, workspace_label
    );
    info!(
        "bot: @{} (id: {}) | private text chats only | send /start in a private chat",
        bot_info.username.as_deref().unwrap_or("(no username)"),
        bot_info.id,
    );

    let mut session_manager = ChatSessionManager::new(
        route,
        configured_default_model,
        api_key,
        options,
        ctx.workspace_root.clone(),
        secrets.clone(),
        allowed_dm_username,
        bound_dm_user_id,
        Some(managed_service_env_path().ok()).flatten(),
    );
    let mut offset = 0i64;
    let mut polling_retries = 0usize;

    info!("telegram polling started");

    loop {
        session_manager.collect_finished_tasks();
        match adapter.get_updates(
            Some(offset),
            Some(POLL_TIMEOUT_SECS),
            Some(&["message", "callback_query"]),
        ) {
            Ok(updates) => {
                debug!(
                    "get_updates call succeeded, returned {} updates",
                    updates.len()
                );
                if polling_retries > 0 {
                    info!(
                        "telegram polling recovered after {} retries",
                        polling_retries
                    );
                    session_manager.notify_polling_recovered();
                }
                polling_retries = 0;
                for update in updates {
                    offset = update.update_id + 1;

                    if let Some(callback) = &update.callback_query {
                        route_callback_query(&adapter, &mut session_manager, callback);
                        continue;
                    }

                    let Some(ref msg) = update.message else { continue };
                    route_message(
                        &adapter,
                        &mut session_manager,
                        &ctx,
                        &secrets,
                        msg,
                        &workspace_label,
                    );
                }
            }
            Err(e) => {
                polling_retries += 1;
                session_manager.notify_polling_retry();
                let backoff = std::cmp::min(5 * polling_retries as u64, 30);
                if polling_retries <= 3 {
                    warn!(
                        "telegram polling failed: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                } else {
                    error!(
                        "telegram polling sustained failure: {}. Retrying in {}s (attempt {}).",
                        e, backoff, polling_retries
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(backoff));
            }
        }
    }
}

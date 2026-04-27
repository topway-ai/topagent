use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
use topagent_core::{
    context::ExecutionContext, model::ModelRoute, Agent, ApprovalEntry, ApprovalMailbox,
    ApprovalMailboxMode, CancellationToken, Message, ProgressCallback, ProgressUpdate,
    RuntimeOptions, TelegramAdapter, WorkspaceRunSnapshotStore,
};
use topagent_core::{
    AccessMode, CapabilityGrant, CapabilityManager, CapabilityProfile, GrantScope,
};
use tracing::{error, info, warn};

use crate::config::defaults::TELEGRAM_BOUND_DM_USER_ID_KEY;
use crate::managed_files::read_managed_env_metadata;
use crate::memory::{promote_verified_task, PromotionContext, WorkspaceMemory};
use crate::progress::LiveProgress;
use crate::run_context::{
    build_agent, prepare_run_context, prepare_workspace_memory, PreparedRunContext,
};
use crate::telegram::approval::{approval_reply_markup, format_approval_resolution};
use crate::telegram::delivery::{send_telegram, send_telegram_with_markup, TelegramOutbound};
use crate::telegram::history::{persist_visible_exchange_to_store, ChatHistoryStore};

use super::admission::DmAdmission;

pub(crate) struct ChatSessionManager {
    pub route: ModelRoute,
    pub configured_default_model: String,
    pub api_key: String,
    pub options: RuntimeOptions,
    pub history_store: ChatHistoryStore,
    pub memory: WorkspaceMemory,
    pub secrets: topagent_core::SecretRegistry,
    pub sessions: HashMap<i64, RunningChatTask>,
    completed_tx: mpsc::Sender<i64>,
    completed_rx: mpsc::Receiver<i64>,
    pub allowed_dm_username: Option<String>,
    pub bound_dm_user_id: Option<i64>,
    env_path: Option<PathBuf>,
    access_manager: CapabilityManager,
}

pub(crate) struct RunningChatTask {
    pub cancel_token: CancellationToken,
    pub progress_callback: Option<ProgressCallback>,
    pub approval_mailbox: ApprovalMailbox,
    #[allow(dead_code)]
    pub instruction: String,
    #[allow(dead_code)]
    pub started_at: std::time::SystemTime,
}

impl ChatSessionManager {
    #[allow(clippy::too_many_arguments, dead_code)]
    pub fn new(
        route: ModelRoute,
        configured_default_model: String,
        api_key: String,
        options: RuntimeOptions,
        workspace_root: PathBuf,
        secrets: topagent_core::SecretRegistry,
        allowed_dm_username: Option<String>,
        bound_dm_user_id: Option<i64>,
        env_path: Option<PathBuf>,
    ) -> Self {
        Self::new_with_access_manager(
            route,
            configured_default_model,
            api_key,
            options,
            workspace_root,
            secrets,
            allowed_dm_username,
            bound_dm_user_id,
            env_path,
            CapabilityManager::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_access_manager(
        route: ModelRoute,
        configured_default_model: String,
        api_key: String,
        options: RuntimeOptions,
        workspace_root: PathBuf,
        secrets: topagent_core::SecretRegistry,
        allowed_dm_username: Option<String>,
        bound_dm_user_id: Option<i64>,
        env_path: Option<PathBuf>,
        access_manager: CapabilityManager,
    ) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        let memory = prepare_workspace_memory(workspace_root.clone());

        Self {
            route,
            configured_default_model,
            api_key,
            options,
            history_store: ChatHistoryStore::new(workspace_root.clone()),
            memory,
            secrets,
            sessions: HashMap::new(),
            completed_tx,
            completed_rx,
            allowed_dm_username,
            bound_dm_user_id,
            env_path,
            access_manager,
        }
    }

    pub(crate) fn create_agent(&self) -> Agent {
        build_agent(&self.route, &self.api_key, self.options.clone())
    }

    pub(crate) fn model_label_for_help(&self) -> String {
        current_model_label_for_help(&self.route, &self.configured_default_model)
    }

    /// Check whether a private-chat sender should be admitted.
    ///
    /// `sender_user_id` is `msg.from.id` (the Telegram user identity), not
    /// `msg.chat.id` (which equals user ID in private chats but could diverge
    /// if the logic ever changes). Using the user ID directly makes the
    /// comparison semantically correct and consistent with what `bind_dm_user_id`
    /// persists.
    pub fn check_dm_admission(
        &self,
        sender_user_id: Option<i64>,
        sender_username: Option<&str>,
    ) -> DmAdmission {
        if let Some(bound_id) = self.bound_dm_user_id {
            if sender_user_id == Some(bound_id) {
                DmAdmission::Allowed
            } else {
                DmAdmission::Denied
            }
        } else if let Some(ref allowed_username) = self.allowed_dm_username {
            if let Some(sender_username) = sender_username {
                // Telegram usernames are case-insensitive
                if sender_username.eq_ignore_ascii_case(allowed_username) {
                    DmAdmission::AllowedFirstBinding
                } else {
                    DmAdmission::Denied
                }
            } else {
                DmAdmission::Denied
            }
        } else {
            DmAdmission::Allowed
        }
    }

    /// Human-readable summary of the current DM admission policy for the /start
    /// help message. Safe to display — never reveals the bound numeric user ID.
    pub(crate) fn dm_access_label(&self) -> String {
        match (&self.allowed_dm_username, self.bound_dm_user_id) {
            (None, _) => "open".to_string(),
            (Some(username), None) => {
                format!(
                    "restricted to @{} (unbound — first match will bind)",
                    username
                )
            }
            (Some(username), Some(_)) => format!("restricted to @{} (bound)", username),
        }
    }

    pub fn bind_dm_user_id(&mut self, user_id: i64) {
        self.bound_dm_user_id = Some(user_id);
        if let Some(ref env_path) = self.env_path {
            if let Ok(values) = read_managed_env_metadata(env_path) {
                let mut updated = values.clone();
                updated.insert(
                    TELEGRAM_BOUND_DM_USER_ID_KEY.to_string(),
                    user_id.to_string(),
                );
                // Keep TELEGRAM_ALLOWED_DM_USERNAME in the env file after
                // binding. Removing it caused a reinstall bug: on the next
                // `topagent install` with the same username, defaults.allowed_dm_username
                // would be None (key gone), so preserved_bound_dm_user_id would
                // compute Some(x) != None → None, silently resetting the binding.
                // With both keys present, the reinstall correctly preserves the
                // binding, and the original policy remains human-readable.
                let _ = crate::service::managed_env::write_managed_env_values(env_path, &updated);
            }
        }
    }

    fn load_redacted_transcript(&self, chat_id: i64) -> Vec<Message> {
        match self.history_store.load(chat_id) {
            Ok(messages) => messages
                .into_iter()
                .map(|message| message.redact_secrets(&self.secrets))
                .collect::<Vec<_>>(),
            Err(err) => {
                warn!(
                    "failed to load Telegram transcript for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
                Vec::new()
            }
        }
    }

    pub(crate) fn build_run_context(
        &self,
        ctx: &ExecutionContext,
        chat_id: i64,
        instruction: &str,
    ) -> PreparedRunContext {
        let transcript = self.load_redacted_transcript(chat_id);
        let prepared = prepare_run_context(ctx, &self.memory, instruction, Some(&transcript));
        PreparedRunContext {
            run_ctx: prepared
                .run_ctx
                .with_workspace_run_snapshot_store(WorkspaceRunSnapshotStore::new(
                    ctx.workspace_root.clone(),
                ))
                .with_capability_manager(self.access_manager.clone())
                .with_session_id(format!("telegram-chat-{chat_id}")),
            loaded_procedure_files: prepared.loaded_procedure_files,
        }
    }

    #[cfg(test)]
    pub fn persist_agent_history(&self, chat_id: i64, agent: &Agent) {
        crate::telegram::history::persist_agent_history_to_store(
            &self.history_store,
            chat_id,
            agent,
            None,
        );
    }

    pub(crate) fn collect_finished_tasks(&mut self) {
        while let Ok(chat_id) = self.completed_rx.try_recv() {
            self.sessions.remove(&chat_id);
        }
    }

    pub(crate) fn is_task_running(&self, chat_id: i64) -> bool {
        self.sessions.contains_key(&chat_id)
    }

    pub(crate) fn stop_chat(&mut self, chat_id: i64) -> bool {
        let Some(task) = self.sessions.get(&chat_id) else {
            return false;
        };

        task.approval_mailbox
            .expire_pending("task stopped by operator");
        task.cancel_token.cancel();
        if let Some(callback) = &task.progress_callback {
            callback(ProgressUpdate::stopping());
        }
        true
    }

    pub(crate) fn notify_polling_retry(&self) {
        self.broadcast_progress(ProgressUpdate::retrying(
            "Telegram polling failed, retrying connection...",
        ));
    }

    pub(crate) fn notify_polling_recovered(&self) {
        self.broadcast_progress(ProgressUpdate::working(
            "Telegram connection restored. Task still running...",
        ));
    }

    fn broadcast_progress(&self, update: ProgressUpdate) {
        for task in self.sessions.values() {
            if let Some(callback) = &task.progress_callback {
                callback(update.clone());
            }
        }
    }

    pub fn reset_chat(&mut self, chat_id: i64) {
        if let Some(task) = self.sessions.remove(&chat_id) {
            task.approval_mailbox
                .supersede_pending("chat reset before approval was resolved");
        }
        match self.history_store.clear(chat_id) {
            Ok(true) => {
                info!(
                    "cleared Telegram transcript for chat {} from {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display()
                );
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    "failed to clear Telegram transcript for chat {} from {}: {}",
                    chat_id,
                    self.history_store.path_for_chat(chat_id).display(),
                    err
                );
            }
        }
    }

    fn pending_approvals(&self, chat_id: i64) -> Vec<ApprovalEntry> {
        self.sessions
            .get(&chat_id)
            .map(|task| task.approval_mailbox.pending())
            .unwrap_or_default()
    }

    pub(crate) fn pending_approvals_reply(&self, chat_id: i64) -> String {
        let approvals = self.pending_approvals(chat_id);
        if approvals.is_empty() {
            return "No pending approvals for this chat.".to_string();
        }

        let mut reply = String::from("Pending approvals:\n");
        for approval in approvals {
            reply.push_str("- ");
            reply.push_str(&approval.request.render_status_line(approval.state));
            reply.push('\n');
        }
        reply.push_str(
            "\nTap the buttons on the approval message, or reply with /approve <id> or /deny <id>.",
        );
        reply
    }

    pub(crate) fn access_command_reply(&mut self, argument: &str) -> String {
        let parts = argument.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            [] | ["status"] => crate::access::render_access_status(&self.access_manager),
            ["set", profile] => match profile.parse::<CapabilityProfile>() {
                Ok(profile) => {
                    let warning = if profile == CapabilityProfile::Full {
                        "WARNING: full access enables broad local filesystem, shell, and network access. High-impact actions still require explicit approval.\n"
                    } else {
                        ""
                    };
                    self.access_manager
                        .set_profile(profile, format!("set from Telegram to {profile}"));
                    format!("{warning}Access profile set to {profile}.")
                }
                Err(err) => err,
            },
            ["grant", target, mode] => self.create_telegram_grant(target, mode, GrantScope::Session),
            ["grant", target, mode, "--scope", scope] => match scope.parse::<GrantScope>() {
                Ok(scope) => self.create_telegram_grant(target, mode, scope),
                Err(err) => err,
            },
            ["revoke", target] => {
                let removed = self
                    .access_manager
                    .revoke_grants_for_target(&crate::access::normalize_target(target));
                if removed == 0 {
                    format!("No grants matched {target}.")
                } else {
                    format!("Revoked {removed} grant(s) matching {target}.")
                }
            }
            ["lockdown"] => {
                self.access_manager.lockdown();
                "Lockdown activated: profile is workspace, network/computer_use are disabled, and grants were cleared.".to_string()
            }
            ["audit"] => match crate::operational_paths::access_audit_path()
                .map(topagent_core::CapabilityAuditLog::new)
                .and_then(|audit| audit.read_recent(10).map_err(Into::into))
            {
                Ok(records) if records.is_empty() => "No access audit records.".to_string(),
                Ok(records) => records
                    .into_iter()
                    .map(|record| {
                        format!(
                            "{} {:?} {} {}",
                            record.timestamp_unix, record.event, record.decision, record.reason
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(err) => format!("Could not read access audit: {err}"),
            },
            _ => "Usage: /access status | /access set developer | /access set full | /access grant <target> <mode> [--scope once|task|path|session|permanent] | /access revoke <target> | /access audit | /access lockdown".to_string(),
        }
    }

    fn create_telegram_grant(&self, target: &str, mode: &str, scope: GrantScope) -> String {
        let mode = match mode.parse::<AccessMode>() {
            Ok(mode) => mode,
            Err(err) => return err,
        };
        let grant = CapabilityGrant::new(
            crate::access::infer_kind(target),
            crate::access::normalize_target(target),
            mode,
            scope,
            "operator-created Telegram grant",
        )
        .persisted(scope == GrantScope::Permanent);
        let id = grant.id.clone();
        self.access_manager.add_grant(grant);
        format!("Created {scope} grant {id} for {mode} access to {target}.")
    }

    fn resolve_approval_request(
        &self,
        chat_id: i64,
        id: &str,
        approve: bool,
        scope: Option<GrantScope>,
        note: &str,
    ) -> std::result::Result<ApprovalEntry, String> {
        let Some(task) = self.sessions.get(&chat_id) else {
            return Err("No task is currently running in this chat.".to_string());
        };

        let result = if approve {
            if let Some(scope) = scope {
                task.approval_mailbox
                    .approve_with_scope(id, scope, Some(note.to_string()))
            } else {
                task.approval_mailbox.approve(id, Some(note.to_string()))
            }
        } else {
            task.approval_mailbox.deny(id, Some(note.to_string()))
        };

        match result {
            Ok(entry) => Ok(entry),
            Err(err) => Err(format!("Could not update approval {}: {}", id, err)),
        }
    }

    pub(crate) fn resolve_approval_command(
        &self,
        chat_id: i64,
        argument: &str,
        approve: bool,
    ) -> String {
        let mut parts = argument.split_whitespace();
        let id = parts.next().unwrap_or("");
        if id.is_empty() {
            return if approve {
                "Usage: /approve <id> [once|task|path|session|permanent]".to_string()
            } else {
                "Usage: /deny <id>".to_string()
            };
        }
        let scope = parts
            .next()
            .and_then(|value| value.parse::<GrantScope>().ok());

        match self.resolve_approval_request(
            chat_id,
            id,
            approve,
            scope,
            "resolved from Telegram command",
        ) {
            Ok(entry) => format_approval_resolution(&entry, approve),
            Err(err) => err,
        }
    }

    pub(crate) fn resolve_approval_callback(
        &self,
        chat_id: i64,
        id: &str,
        approve: bool,
        scope: Option<GrantScope>,
    ) -> std::result::Result<ApprovalEntry, String> {
        self.resolve_approval_request(chat_id, id, approve, scope, "resolved from Telegram button")
    }

    pub(crate) fn start_message(
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
        let mut agent = self.create_agent();

        let cancel_token = CancellationToken::new();
        let task_id = format!("telegram-{chat_id}-{}", unix_now());
        let approval_mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Wait);
        let approval_adapter = adapter.clone();
        let approval_secrets = self.secrets.clone();
        approval_mailbox.set_notifier(Arc::new(move |request| {
            let mut message = request.render_details();
            if request.capability.is_some() {
                message.push_str(&format!(
                    "\n\nTap a scope button below, or reply with /approve {} once|task|path|session. Use /deny {} to reject.",
                    request.id, request.id
                ));
            } else {
                message.push_str(&format!(
                    "\n\nTap Approve or Deny below, or reply with /approve {} or /deny {}.",
                    request.id, request.id
                ));
            }
            let chunks = topagent_core::channel::telegram::chunk_text(&message, 4000);
            let reply_markup = approval_reply_markup(&request);
            send_telegram_with_markup(
                &approval_adapter,
                chat_id,
                chunks,
                Some(&reply_markup),
                Some(&approval_secrets),
            );
        }));
        let prepared_run = self.build_run_context(
            &ctx.clone()
                .with_cancel_token(cancel_token.clone())
                .with_approval_mailbox(approval_mailbox.clone())
                .with_task_id(task_id.clone()),
            chat_id,
            text,
        );
        let loaded_procedure_files = prepared_run.loaded_procedure_files.clone();
        let run_ctx = prepared_run.run_ctx;
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
        let history_store = self.history_store.clone();
        let memory = self.memory.clone();
        let worker_secrets = self.secrets.clone();
        let adapter = adapter.clone();
        let instruction = text.to_string();
        let distill_options = self.options.clone();
        let access_manager = self.access_manager.clone();
        let worker_task_id = task_id.clone();

        thread::spawn(move || {
            let has_progress = worker_progress_callback.is_some();
            let mut promotion_notes = Vec::new();
            if let Some(callback) = &worker_progress_callback {
                agent.set_progress_callback(Some(callback.clone()));
            }

            let result = agent.run(&run_ctx, &instruction);
            agent.set_progress_callback(None);
            if let Ok(_response) = &result {
                if let Some(task_result) = agent.last_task_result().cloned() {
                    match agent.plan().lock() {
                        Ok(plan) => match promote_verified_task(&PromotionContext {
                            memory: &memory,
                            ctx: &run_ctx,
                            options: &distill_options,
                            instruction: &instruction,
                            task_mode: agent.task_mode(),
                            task_result: &task_result,
                            plan: &plan.clone(),
                            durable_memory_written: agent.durable_memory_written_this_run(),
                            loaded_procedure_files: &loaded_procedure_files,
                        }) {
                            Ok(report) => {
                                if report.note_file.is_some()
                                    || report.procedure_file.is_some()
                                    || report.trajectory_file.is_some()
                                {
                                    info!(
                                        note = report.note_file.as_deref().unwrap_or(""),
                                        procedure = report.procedure_file.as_deref().unwrap_or(""),
                                        trajectory =
                                            report.trajectory_file.as_deref().unwrap_or(""),
                                        chat_id,
                                        "saved promoted workspace learning artifacts"
                                    );
                                }
                                promotion_notes = report.notes;
                            }
                            Err(err) => {
                                warn!("failed to promote verified Telegram task memory: {}", err)
                            }
                        },
                        Err(err) => {
                            warn!("failed to lock agent plan for Telegram promotion: {}", err)
                        }
                    }
                }
            }
            if let Some(progress) = progress {
                progress.wait();
            }

            handle_telegram_run_outcome(
                result,
                promotion_notes,
                has_progress,
                &adapter,
                chat_id,
                &instruction,
                &history_store,
                &worker_secrets,
            );

            access_manager.clear_task_temporary_grants(&worker_task_id);

            let _ = completed_tx.send(chat_id);
        });

        self.sessions.insert(
            chat_id,
            RunningChatTask {
                cancel_token,
                progress_callback,
                approval_mailbox,
                instruction: text.to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );
        Vec::new()
    }
}

/// Shared outcome handler for Telegram worker threads.
///
/// Centralises the three distinct terminal states of a Telegram run:
/// - Ok(response)       — delivered successfully; persist the exchange.
/// - Err(Stopped)       — user cancelled; persist instruction only (no reply).
/// - Err(other)         — runtime error; send error text when no live progress
///   is running; always persist instruction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_telegram_run_outcome(
    result: topagent_core::Result<String>,
    promotion_notes: Vec<String>,
    has_progress: bool,
    adapter: &dyn TelegramOutbound,
    chat_id: i64,
    instruction: &str,
    history_store: &ChatHistoryStore,
    worker_secrets: &topagent_core::SecretRegistry,
) {
    match result {
        Ok(mut response) => {
            if !promotion_notes.is_empty() {
                response.push_str("\n\n### Trust Notes\n");
                for note in promotion_notes {
                    response.push_str(&format!("- {}\n", note));
                }
            }
            let max_len = 4000;
            let chunks = if response.len() <= max_len {
                vec![response.clone()]
            } else {
                topagent_core::channel::telegram::chunk_text(&response, max_len)
            };
            let delivery = send_telegram(adapter, chat_id, chunks, Some(worker_secrets));
            persist_visible_exchange_to_store(
                history_store,
                chat_id,
                instruction,
                delivery.fully_delivered().then_some(response.as_str()),
            );
        }
        Err(topagent_core::Error::Stopped(_)) => {
            persist_visible_exchange_to_store(history_store, chat_id, instruction, None);
        }
        Err(e) => {
            if !has_progress {
                let error_text = format!("Error: {}", e);
                let delivery = send_telegram(
                    adapter,
                    chat_id,
                    vec![error_text.clone()],
                    Some(worker_secrets),
                );
                persist_visible_exchange_to_store(
                    history_store,
                    chat_id,
                    instruction,
                    delivery.fully_delivered().then_some(error_text.as_str()),
                );
            } else {
                persist_visible_exchange_to_store(history_store, chat_id, instruction, None);
            }
        }
    }
}

pub(crate) fn current_model_label_for_help(
    active_route: &ModelRoute,
    configured_default_model: &str,
) -> String {
    let configured_default_model = configured_default_model.trim();
    if configured_default_model.is_empty() || configured_default_model == active_route.model_id {
        active_route.model_id.clone()
    } else {
        format!(
            "{} (override; default {})",
            active_route.model_id, configured_default_model
        )
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::{TELEGRAM_ALLOWED_DM_USERNAME_KEY, TOPAGENT_SERVICE_MANAGED_KEY};
    use crate::memory::procedures::{save_procedure, ProcedureDraft};
    use crate::memory::{
        MEMORY_PROCEDURES_RELATIVE_DIR, MEMORY_TRAJECTORIES_RELATIVE_DIR,
        TELEGRAM_HISTORY_RELATIVE_DIR,
    };
    use crate::telegram::history::{persist_agent_history_to_store, ChatHistoryStore};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use topagent_core::channel::telegram::{ChannelError, TelegramInlineKeyboardMarkup};
    use topagent_core::{
        ApprovalCheck, ApprovalRequestDraft, ApprovalTriggerKind, BehaviorContract,
        CancellationToken, Message, ModelRoute, ProgressKind, ProgressUpdate,
    };

    fn test_manager(workspace_root: PathBuf) -> ChatSessionManager {
        ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace_root,
            topagent_core::SecretRegistry::new(),
            None,
            None,
            None,
        )
    }

    fn pending_approval_mailbox() -> ApprovalMailbox {
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let check = mailbox.request_decision(
            ApprovalRequestDraft {
                action_kind: ApprovalTriggerKind::GitCommit,
                short_summary: "git commit: ship it".to_string(),
                exact_action: "git_commit(message=\"ship it\")".to_string(),
                reason: "commits publish a durable repo milestone".to_string(),
                scope_of_impact: "Creates a new git commit in the workspace repository."
                    .to_string(),
                expected_effect: "Staged changes become a durable repo milestone.".to_string(),
                rollback_hint: Some(
                    "Use git revert or git reset if the commit was mistaken.".to_string(),
                ),
                capability: None,
            },
            None,
        );
        assert!(matches!(check, ApprovalCheck::Pending(_)));
        mailbox
    }

    #[test]
    fn test_access_grant_command_normalizes_home_targets() {
        let temp = TempDir::new().unwrap();
        let mut manager = test_manager(temp.path().to_path_buf());
        let reply = manager.access_command_reply("grant ~/Downloads read --scope task");
        assert!(reply.contains("Created task grant"));

        let expected = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Downloads").display().to_string())
            .unwrap_or_else(|| "~/Downloads".to_string());
        let grants = manager.access_manager.grants();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].target, expected);
        assert_eq!(grants[0].mode, AccessMode::Read);
        assert_eq!(grants[0].scope, GrantScope::ThisTask);

        let reply = manager.access_command_reply("revoke ~/Downloads");
        assert!(reply.contains("Revoked 1 grant"));
        assert!(manager.access_manager.grants().is_empty());
    }

    #[derive(Debug, Default)]
    struct TestTelegramOutbound {
        sent: Mutex<Vec<String>>,
    }

    impl TestTelegramOutbound {
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl TelegramOutbound for TestTelegramOutbound {
        fn send_message_to_chat(&self, _chat_id: i64, text: &str) -> Result<(), ChannelError> {
            self.sent.lock().unwrap().push(text.to_string());
            Ok(())
        }

        fn send_message_to_chat_with_markup(
            &self,
            _chat_id: i64,
            text: &str,
            _reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        ) -> Result<(), ChannelError> {
            self.sent.lock().unwrap().push(text.to_string());
            Ok(())
        }

        fn answer_callback_query(
            &self,
            _callback_query_id: &str,
            _text: Option<&str>,
            _show_alert: bool,
        ) -> Result<(), ChannelError> {
            Ok(())
        }

        fn edit_message_text(
            &self,
            _chat_id: i64,
            _message_id: i64,
            _text: &str,
            _reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        ) -> Result<(), ChannelError> {
            Ok(())
        }

        fn acknowledge(&self, _chat_id: i64, _message_id: i64) -> Result<(), ChannelError> {
            Ok(())
        }
    }

    #[test]
    fn test_stop_chat_cancels_running_task_and_emits_stopping_progress() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let cancel_token = CancellationToken::new();
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
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
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        assert!(!manager.stop_chat(42));
    }

    #[test]
    fn test_help_model_label_shows_override_and_default_when_they_differ() {
        let label = current_model_label_for_help(
            &ModelRoute::openrouter("anthropic/claude-sonnet-4.6"),
            "qwen/qwen3.6-plus:free",
        );

        assert_eq!(
            label,
            "anthropic/claude-sonnet-4.6 (override; default qwen/qwen3.6-plus:free)"
        );
    }

    #[test]
    fn test_help_model_label_falls_back_to_active_route_when_it_matches_default() {
        let label = current_model_label_for_help(
            &ModelRoute::openrouter("anthropic/claude-sonnet-4.6"),
            "anthropic/claude-sonnet-4.6",
        );

        assert_eq!(label, "anthropic/claude-sonnet-4.6");
    }

    #[test]
    fn test_pending_approvals_reply_lists_request_ids() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: pending_approval_mailbox(),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        let reply = manager.pending_approvals_reply(42);
        assert!(reply.contains("Pending approvals"));
        assert!(reply.contains("apr-1"));
        assert!(reply.contains("Tap the buttons"));
        assert!(reply.contains("/approve <id>"));
    }

    #[test]
    fn test_resolve_approval_command_updates_pending_request() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mailbox = pending_approval_mailbox();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: mailbox.clone(),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        let reply = manager.resolve_approval_command(42, "apr-1", true);
        assert!(reply.contains("Approval apr-1 approved"));
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            topagent_core::ApprovalState::Approved
        );
    }

    #[test]
    fn test_resolve_approval_callback_updates_pending_request() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mailbox = pending_approval_mailbox();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: mailbox.clone(),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        let entry = manager
            .resolve_approval_callback(42, "apr-1", false, None)
            .unwrap();
        assert_eq!(entry.state, topagent_core::ApprovalState::Denied);
        assert_eq!(
            mailbox.get("apr-1").unwrap().state,
            topagent_core::ApprovalState::Denied
        );
    }

    #[test]
    fn test_notify_polling_retry_emits_retrying_progress_to_running_chat() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
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
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let updates = Arc::new(Mutex::new(Vec::<ProgressUpdate>::new()));
        let sink = updates.clone();
        let progress_callback: topagent_core::ProgressCallback = Arc::new(move |update| {
            sink.lock().unwrap().push(update);
        });

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: Some(progress_callback),
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
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

    #[test]
    fn test_memory_context_retrieves_targeted_transcript_snippet_instead_of_restoring_whole_history(
    ) {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4242;
        let original_manager = test_manager(workspace.path().to_path_buf());
        let mut original_agent = original_manager.create_agent();
        original_agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: maple comet."),
            Message::assistant("Stored. I will remember maple comet."),
            Message::user("Also keep cedar echo."),
            Message::assistant("Stored. I will remember cedar echo."),
        ]);
        persist_agent_history_to_store(
            &original_manager.history_store,
            chat_id,
            &original_agent,
            None,
        );

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let prepared_run = restarted_manager.build_run_context(
            &ExecutionContext::new(workspace.path().to_path_buf()),
            chat_id,
            "What was the maple phrase I mentioned earlier?",
        );
        let memory_context = prepared_run.run_ctx.memory_context().unwrap();

        assert!(memory_context.contains("maple comet"));
        assert!(!memory_context.contains("cedar echo"));
        assert!(workspace
            .path()
            .join(TELEGRAM_HISTORY_RELATIVE_DIR)
            .join("chat-4242.json")
            .is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(
                workspace
                    .path()
                    .join(TELEGRAM_HISTORY_RELATIVE_DIR)
                    .join("chat-4242.json"),
            )
            .unwrap()
            .permissions()
            .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn test_telegram_build_run_context_matches_one_shot_context_and_keeps_prompt_targeted() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 4243;
        let manager = test_manager(workspace.path().to_path_buf());

        std::fs::create_dir_all(workspace.path().join(".topagent")).unwrap();
        std::fs::write(
            workspace.path().join(".topagent/USER.md"),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
                steps: vec![
                    "Inspect the mailbox.".to_string(),
                    "Compact safely.".to_string(),
                ],
                pitfalls: vec!["Do not drop pending approvals.".to_string()],
                verification: "cargo test -p topagent-core approval".to_string(),
                source_task: Some("approval mailbox compaction".to_string()),
                source_note: None,
                source_trajectory: None,
                supersedes: None,
            },
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Irrelevant visual polish flow".to_string(),
                when_to_use: "Use for unrelated diagram theming.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
                steps: vec!["Adjust the diagram palette.".to_string()],
                pitfalls: vec!["Do not use for backend repair tasks.".to_string()],
                verification: "cargo test -p topagent-cli".to_string(),
                source_task: Some("visual polish".to_string()),
                source_note: None,
                source_trajectory: None,
                supersedes: None,
            },
        )
        .unwrap();
        std::fs::create_dir_all(workspace.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR)).unwrap();
        std::fs::write(
            workspace
                .path()
                .join(MEMORY_TRAJECTORIES_RELATIVE_DIR)
                .join("ignored.json"),
            r#"{"task_intent":"ignored trajectory"}"#,
        )
        .unwrap();
        manager.memory.consolidate_memory_if_needed().unwrap();
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Remember the maple compaction phrase.",
            Some("Stored. Maple compaction phrase recorded."),
        );
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Also remember the cedar orbit token.",
            Some("Stored. Cedar orbit token recorded."),
        );

        let base_ctx = ExecutionContext::new(workspace.path().to_path_buf());
        let telegram_prepared = manager.build_run_context(
            &base_ctx,
            chat_id,
            "what was the maple phrase I mentioned earlier while repairing approval mailbox compaction?",
        );
        let transcript = manager.history_store.load(chat_id).unwrap();
        let one_shot_prepared = prepare_run_context(
            &base_ctx,
            &manager.memory,
            "what was the maple phrase I mentioned earlier while repairing approval mailbox compaction?",
            Some(&transcript),
        );

        assert_eq!(
            telegram_prepared.loaded_procedure_files,
            one_shot_prepared.loaded_procedure_files
        );
        assert_eq!(
            telegram_prepared.run_ctx.memory_context(),
            one_shot_prepared.run_ctx.memory_context()
        );
        assert_eq!(
            telegram_prepared.run_ctx.operator_context(),
            one_shot_prepared.run_ctx.operator_context()
        );
        assert_eq!(
            telegram_prepared.run_ctx.run_trust_context(),
            one_shot_prepared.run_ctx.run_trust_context()
        );

        let memory_context = telegram_prepared
            .run_ctx
            .memory_context()
            .unwrap_or_default();
        assert!(memory_context.contains("Approval mailbox compaction playbook"));
        assert!(memory_context.contains("maple compaction phrase"));
        assert!(!memory_context.contains("cedar orbit token"));
        assert!(!memory_context.contains("ignored trajectory"));
        assert!(
            telegram_prepared.loaded_procedure_files.len()
                <= BehaviorContract::default().memory.max_procedures_to_load
        );
    }

    #[test]
    fn test_history_is_saved_to_disk_as_user_visible_transcript_only() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 777;
        let manager = test_manager(workspace.path().to_path_buf());
        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: cedar echo."),
            Message::tool_request("tool-1", "bash", serde_json::json!({"command": "pwd"})),
            Message::tool_result("tool-1", "/tmp/workspace"),
            Message::assistant("Stored. I will remember cedar echo."),
        ]);

        persist_agent_history_to_store(&manager.history_store, chat_id, &agent, None);

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(
            persisted[0].as_text(),
            Some("Remember this exact phrase: cedar echo.")
        );
        assert_eq!(
            persisted[1].as_text(),
            Some("Stored. I will remember cedar echo.")
        );
    }

    #[test]
    fn test_persist_visible_exchange_stores_only_delivered_messages() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 1717;
        let manager = test_manager(workspace.path().to_path_buf());

        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Fix the config path.",
            Some("Done. Verified with cargo test."),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].as_text(), Some("Fix the config path."));
        assert_eq!(
            persisted[1].as_text(),
            Some("Done. Verified with cargo test.")
        );
    }

    #[test]
    fn test_persist_visible_exchange_skips_undelivered_assistant_reply() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 1818;
        let manager = test_manager(workspace.path().to_path_buf());

        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Fix the config path.",
            Some("Done. Verified with cargo test."),
        );
        persist_visible_exchange_to_store(
            &manager.history_store,
            chat_id,
            "Run it again after the restart.",
            None,
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 3);
        assert_eq!(persisted[0].as_text(), Some("Fix the config path."));
        assert_eq!(
            persisted[1].as_text(),
            Some("Done. Verified with cargo test.")
        );
        assert_eq!(
            persisted[2].as_text(),
            Some("Run it again after the restart.")
        );
    }

    #[test]
    fn test_post_restart_persist_keeps_pre_restart_exchange_in_file() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 5150;
        let original_manager = test_manager(workspace.path().to_path_buf());
        let mut original_agent = original_manager.create_agent();
        original_agent.restore_conversation_messages(vec![
            Message::user("Remember this exact phrase: lunar pine."),
            Message::assistant("Stored. I will remember lunar pine."),
        ]);
        persist_agent_history_to_store(
            &original_manager.history_store,
            chat_id,
            &original_agent,
            None,
        );

        let restarted_manager = test_manager(workspace.path().to_path_buf());
        let mut next_agent = restarted_manager.create_agent();
        next_agent.restore_conversation_messages(vec![
            Message::user("What exact phrase did I ask you to remember before the restart?"),
            Message::assistant("lunar pine"),
        ]);

        persist_agent_history_to_store(
            &restarted_manager.history_store,
            chat_id,
            &next_agent,
            None,
        );

        let persisted = restarted_manager.history_store.load(chat_id).unwrap();
        assert_eq!(persisted.len(), 4);
        assert_eq!(
            persisted[0].as_text(),
            Some("Remember this exact phrase: lunar pine.")
        );
        assert_eq!(
            persisted[1].as_text(),
            Some("Stored. I will remember lunar pine.")
        );
        assert_eq!(
            persisted[2].as_text(),
            Some("What exact phrase did I ask you to remember before the restart?")
        );
        assert_eq!(persisted[3].as_text(), Some("lunar pine"));
    }

    #[test]
    fn test_reset_chat_clears_persisted_history_file() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 9001;
        let mut manager = test_manager(workspace.path().to_path_buf());
        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember the answer is 17."),
            Message::assistant("Stored."),
        ]);
        manager.persist_agent_history(chat_id, &agent);
        let history_path = workspace
            .path()
            .join(TELEGRAM_HISTORY_RELATIVE_DIR)
            .join("chat-9001.json");
        let memory_index_path = workspace.path().join(".topagent").join("MEMORY.md");
        assert!(history_path.is_file());
        assert!(memory_index_path.is_file());

        manager.sessions.insert(
            chat_id,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );
        manager.reset_chat(chat_id);

        assert!(!history_path.exists());
        assert!(memory_index_path.exists());
        assert!(!manager.sessions.contains_key(&chat_id));
    }

    #[test]
    fn test_reset_chat_preserves_operator_and_workspace_memory_artifacts() {
        let workspace = TempDir::new().unwrap();
        let chat_id = 9102;
        let mut manager = test_manager(workspace.path().to_path_buf());

        std::fs::create_dir_all(workspace.path().join(".topagent")).unwrap();
        std::fs::write(
            workspace.path().join(".topagent/USER.md"),
            "# Operator Model\n\n## concise_final_answers\n**Category:** response_style\n**Updated:** <t:1>\n**Preference:** Keep final answers concise.\n",
        )
        .unwrap();
        save_procedure(
            &workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR),
            &ProcedureDraft {
                title: "Approval mailbox compaction playbook".to_string(),
                when_to_use: "Use for approval mailbox compaction work.".to_string(),
                prerequisites: vec!["Stay inside the workspace.".to_string()],
                steps: vec![
                    "Inspect the mailbox.".to_string(),
                    "Compact safely.".to_string(),
                ],
                pitfalls: vec!["Do not drop pending approvals.".to_string()],
                verification: "cargo test -p topagent-core approval".to_string(),
                source_task: Some("approval mailbox compaction".to_string()),
                source_note: None,
                source_trajectory: None,
                supersedes: None,
            },
        )
        .unwrap();
        manager.memory.consolidate_memory_if_needed().unwrap();

        let mut agent = manager.create_agent();
        agent.restore_conversation_messages(vec![
            Message::user("Remember the answer is 17."),
            Message::assistant("Stored."),
        ]);
        manager.persist_agent_history(chat_id, &agent);

        let transcript_path = workspace
            .path()
            .join(TELEGRAM_HISTORY_RELATIVE_DIR)
            .join("chat-9102.json");
        let user_path = workspace.path().join(".topagent").join("USER.md");
        let memory_index_path = workspace.path().join(".topagent").join("MEMORY.md");
        let procedure_dir = workspace.path().join(MEMORY_PROCEDURES_RELATIVE_DIR);
        assert!(transcript_path.exists());
        assert!(user_path.exists());
        assert!(memory_index_path.exists());
        assert!(procedure_dir.is_dir());

        manager.reset_chat(chat_id);

        assert!(!transcript_path.exists());
        assert!(user_path.exists());
        assert!(memory_index_path.exists());
        assert!(procedure_dir.is_dir());
    }

    #[test]
    fn test_check_dm_admission_is_case_insensitive() {
        let workspace = TempDir::new().unwrap();
        let manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("someuser".to_string()),
            None,
            None,
        );

        assert!(matches!(
            manager.check_dm_admission(Some(42), Some("someuser")),
            DmAdmission::AllowedFirstBinding
        ));
        assert!(matches!(
            manager.check_dm_admission(Some(42), Some("SomeUser")),
            DmAdmission::AllowedFirstBinding
        ));
        assert!(matches!(
            manager.check_dm_admission(Some(42), Some("SOMEUSER")),
            DmAdmission::AllowedFirstBinding
        ));
        assert!(matches!(
            manager.check_dm_admission(Some(42), Some("other")),
            DmAdmission::Denied
        ));
        assert!(matches!(
            manager.check_dm_admission(Some(42), None),
            DmAdmission::Denied
        ));
    }

    #[test]
    fn test_clear_workspace_telegram_history_removes_all_chat_transcripts() {
        let workspace = TempDir::new().unwrap();
        let history_store = ChatHistoryStore::new(workspace.path().to_path_buf());
        history_store
            .save(101, &[Message::user("hello"), Message::assistant("world")])
            .unwrap();
        history_store
            .save(202, &[Message::user("goodbye"), Message::assistant("moon")])
            .unwrap();

        let history_dir = workspace.path().join(TELEGRAM_HISTORY_RELATIVE_DIR);
        assert!(history_dir.is_dir());

        let cleared =
            crate::telegram::history::clear_workspace_telegram_history(workspace.path()).unwrap();

        assert!(cleared);
        assert!(!history_dir.exists());
    }

    #[test]
    fn test_bind_dm_user_id_preserves_allowed_username_key_in_env_file() {
        use crate::managed_files::read_managed_env_metadata;
        use crate::service::managed_env::write_managed_env_values;
        use std::collections::HashMap;
        use tempfile::NamedTempFile;

        let env_file = NamedTempFile::new().unwrap();
        let env_path = env_file.path().to_path_buf();

        let initial = HashMap::from([
            (
                TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "operator".to_string(),
            ),
            (TOPAGENT_SERVICE_MANAGED_KEY.to_string(), "1".to_string()),
        ]);
        write_managed_env_values(&env_path, &initial).unwrap();

        let workspace = TempDir::new().unwrap();
        let mut manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("operator".to_string()),
            None,
            Some(env_path.clone()),
        );

        manager.bind_dm_user_id(424242);

        let values = read_managed_env_metadata(&env_path).unwrap();
        assert_eq!(
            values
                .get(TELEGRAM_BOUND_DM_USER_ID_KEY)
                .map(String::as_str),
            Some("424242"),
            "bound user ID must be written"
        );
        assert_eq!(
            values
                .get(TELEGRAM_ALLOWED_DM_USERNAME_KEY)
                .map(String::as_str),
            Some("operator"),
            "allowed username must be preserved (not deleted) after binding"
        );
    }

    #[test]
    fn test_bind_dm_user_id_preserving_username_means_reinstall_preserves_binding() {
        use crate::managed_files::read_managed_env_metadata;
        use crate::service::managed_env::write_managed_env_values;
        use std::collections::HashMap;
        use tempfile::NamedTempFile;

        let env_file = NamedTempFile::new().unwrap();
        let env_path = env_file.path().to_path_buf();

        let initial = HashMap::from([
            (
                TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "operator".to_string(),
            ),
            (TOPAGENT_SERVICE_MANAGED_KEY.to_string(), "1".to_string()),
        ]);
        write_managed_env_values(&env_path, &initial).unwrap();

        let workspace = TempDir::new().unwrap();
        let mut manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("operator".to_string()),
            None,
            Some(env_path.clone()),
        );
        manager.bind_dm_user_id(777);

        let values = read_managed_env_metadata(&env_path).unwrap();
        let defaults = crate::config::defaults::TelegramModeDefaults::from_metadata(&values);

        assert_eq!(
            defaults.allowed_dm_username.as_deref(),
            Some("operator"),
            "username must round-trip so install.rs can compare it"
        );
        assert_eq!(
            defaults.bound_dm_user_id,
            Some(777),
            "bound ID must round-trip"
        );

        let reinstall_username = Some("operator".to_string());
        let preserved = if reinstall_username == defaults.allowed_dm_username {
            defaults.bound_dm_user_id
        } else {
            None
        };
        assert_eq!(
            preserved,
            Some(777),
            "reinstall with same username must preserve the binding"
        );
    }

    #[test]
    fn test_check_dm_admission_uses_user_id_not_chat_id_for_bound_check() {
        let workspace = TempDir::new().unwrap();
        let manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("operator".to_string()),
            Some(424242),
            None,
        );

        assert!(matches!(
            manager.check_dm_admission(Some(424242), Some("operator")),
            DmAdmission::Allowed
        ));
        assert!(matches!(
            manager.check_dm_admission(Some(999999), Some("operator")),
            DmAdmission::Denied,
        ));
        assert!(matches!(
            manager.check_dm_admission(None, Some("operator")),
            DmAdmission::Denied,
        ));
    }

    #[test]
    fn test_dm_access_label_describes_admission_state() {
        let workspace = TempDir::new().unwrap();

        let open_manager = ChatSessionManager::new(
            ModelRoute::openrouter("m"),
            "m".to_string(),
            "k".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            None,
            None,
            None,
        );
        assert_eq!(open_manager.dm_access_label(), "open");

        let unbound_manager = ChatSessionManager::new(
            ModelRoute::openrouter("m"),
            "m".to_string(),
            "k".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("alice".to_string()),
            None,
            None,
        );
        let label = unbound_manager.dm_access_label();
        assert!(
            label.contains("alice") && label.contains("unbound"),
            "expected unbound label, got: {label}"
        );

        let bound_manager = ChatSessionManager::new(
            ModelRoute::openrouter("m"),
            "m".to_string(),
            "k".to_string(),
            RuntimeOptions::default(),
            workspace.path().to_path_buf(),
            topagent_core::SecretRegistry::new(),
            Some("alice".to_string()),
            Some(424242),
            None,
        );
        let bound_label = bound_manager.dm_access_label();
        assert!(
            bound_label.contains("alice") && bound_label.contains("bound"),
            "expected bound label, got: {bound_label}"
        );
        assert!(
            !bound_label.contains("424242"),
            "label must not reveal numeric user ID: {bound_label}"
        );
    }

    #[test]
    fn test_current_model_label_for_help_shows_override_and_default() {
        let label =
            current_model_label_for_help(&ModelRoute::openrouter("minimax/m2"), "minimax/m2");
        assert_eq!(label, "minimax/m2");

        let label = current_model_label_for_help(
            &ModelRoute::openrouter("override/model"),
            "persisted/model",
        );
        assert!(
            label.contains("override/model") && label.contains("persisted/model"),
            "label must show both: {label}"
        );
    }

    #[test]
    fn test_run_telegram_uses_config_admission_fields_not_defaults() {
        use crate::config::defaults::{
            TelegramModeDefaults, TELEGRAM_ALLOWED_DM_USERNAME_KEY, TELEGRAM_BOUND_DM_USER_ID_KEY,
        };
        use std::collections::HashMap;

        let workspace = TempDir::new().unwrap();
        let values = HashMap::from([
            ("OPENROUTER_API_KEY".to_string(), "k".to_string()),
            ("TELEGRAM_BOT_TOKEN".to_string(), "1:t".to_string()),
            (
                crate::config::defaults::TOPAGENT_WORKSPACE_KEY.to_string(),
                workspace.path().display().to_string(),
            ),
            (
                TELEGRAM_ALLOWED_DM_USERNAME_KEY.to_string(),
                "@Operator".to_string(),
            ),
            (TELEGRAM_BOUND_DM_USER_ID_KEY.to_string(), "99".to_string()),
        ]);
        let defaults = TelegramModeDefaults::from_metadata(&values);
        assert_eq!(defaults.allowed_dm_username.as_deref(), Some("operator"));
        assert_eq!(defaults.bound_dm_user_id, Some(99));

        let params = crate::config::defaults::CliParams {
            api_key: None,
            opencode_api_key: None,
            model: None,
            workspace: None,
            max_steps: None,
            max_retries: None,
            timeout_secs: None,
        };
        let config =
            crate::config::runtime::resolve_telegram_mode_config(None, params, defaults.clone())
                .unwrap();

        assert_eq!(config.allowed_dm_username, defaults.allowed_dm_username);
        assert_eq!(config.bound_dm_user_id, defaults.bound_dm_user_id);
    }

    #[test]
    fn test_running_chat_task_carries_instruction_and_start_time() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let before = std::time::SystemTime::now();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "Fix the authentication bug".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        let task = manager.sessions.get(&42).unwrap();
        assert_eq!(task.instruction, "Fix the authentication bug");
        assert!(
            task.started_at >= before,
            "started_at must not predate the test"
        );
    }

    #[test]
    fn test_running_chat_task_instruction_is_preserved_through_stop() {
        let workspace = TempDir::new().unwrap();
        let mut manager = test_manager(workspace.path().to_path_buf());
        let cancel_token = CancellationToken::new();

        manager.sessions.insert(
            42,
            RunningChatTask {
                cancel_token: cancel_token.clone(),
                progress_callback: None,
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "Refactor the config module".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        assert!(manager.stop_chat(42));
        assert!(cancel_token.is_cancelled());
        let task = manager.sessions.get(&42).unwrap();
        assert_eq!(task.instruction, "Refactor the config module");
    }

    #[test]
    fn test_outcome_ok_persists_both_instruction_and_response() {
        let workspace = TempDir::new().unwrap();
        let manager = test_manager(workspace.path().to_path_buf());
        let chat_id = 5001;
        let outbound = TestTelegramOutbound::default();

        handle_telegram_run_outcome(
            Ok("Task complete.".to_string()),
            vec![],
            false,
            &outbound,
            chat_id,
            "Do the thing",
            &manager.history_store,
            &topagent_core::SecretRegistry::new(),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(outbound.sent(), vec!["Task complete.".to_string()]);
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].as_text(), Some("Do the thing"));
        assert_eq!(persisted[1].as_text(), Some("Task complete."));
    }

    #[test]
    fn test_outcome_stopped_persists_only_instruction() {
        let workspace = TempDir::new().unwrap();
        let manager = test_manager(workspace.path().to_path_buf());
        let chat_id = 5002;
        let outbound = TestTelegramOutbound::default();

        handle_telegram_run_outcome(
            Err(topagent_core::Error::Stopped("user cancelled".to_string())),
            vec![],
            false,
            &outbound,
            chat_id,
            "Analyse the logs",
            &manager.history_store,
            &topagent_core::SecretRegistry::new(),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert!(outbound.sent().is_empty());
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].as_text(), Some("Analyse the logs"));
    }

    #[test]
    fn test_outcome_error_with_no_progress_persists_instruction() {
        let workspace = TempDir::new().unwrap();
        let manager = test_manager(workspace.path().to_path_buf());
        let chat_id = 5003;
        let outbound = TestTelegramOutbound::default();

        handle_telegram_run_outcome(
            Err(topagent_core::Error::Provider("network error".to_string())),
            vec![],
            false,
            &outbound,
            chat_id,
            "Run cargo test",
            &manager.history_store,
            &topagent_core::SecretRegistry::new(),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert_eq!(
            outbound.sent(),
            vec!["Error: provider error: network error"]
        );
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].as_text(), Some("Run cargo test"));
        assert_eq!(
            persisted[1].as_text(),
            Some("Error: provider error: network error")
        );
    }

    #[test]
    fn test_outcome_error_with_progress_persists_only_instruction() {
        let workspace = TempDir::new().unwrap();
        let manager = test_manager(workspace.path().to_path_buf());
        let chat_id = 5004;
        let outbound = TestTelegramOutbound::default();

        handle_telegram_run_outcome(
            Err(topagent_core::Error::Provider("timeout".to_string())),
            vec![],
            true,
            &outbound,
            chat_id,
            "Check status",
            &manager.history_store,
            &topagent_core::SecretRegistry::new(),
        );

        let persisted = manager.history_store.load(chat_id).unwrap();
        assert!(outbound.sent().is_empty());
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].as_text(), Some("Check status"));
    }

    #[test]
    fn test_outcome_ok_with_promotion_notes_appends_trust_section() {
        let workspace = TempDir::new().unwrap();
        let store = ChatHistoryStore::new(workspace.path().to_path_buf());
        let chat_id = 5005;
        let outbound = TestTelegramOutbound::default();

        let response = "Done.".to_string();
        handle_telegram_run_outcome(
            Ok(response),
            vec!["Low-trust content noted.".to_string()],
            false,
            &outbound,
            chat_id,
            "Fix auth",
            &store,
            &topagent_core::SecretRegistry::new(),
        );
        let persisted = store.load(chat_id).unwrap();
        let sent = outbound.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("### Trust Notes"));
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].as_text(), Some("Fix auth"));
        assert!(persisted[1]
            .as_text()
            .unwrap_or_default()
            .contains("Low-trust content noted."));
    }
}

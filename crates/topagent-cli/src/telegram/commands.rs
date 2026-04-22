use crate::commands::surface::{
    ParsedTelegramCommand, TelegramCommandKind, PRODUCT_NAME, TELEGRAM_COMMANDS,
};
use crate::telegram::session::ChatSessionManager;

pub(super) fn handle_parsed_command(
    command: ParsedTelegramCommand<'_>,
    session_manager: &mut ChatSessionManager,
    chat_id: i64,
    workspace_label: &str,
) -> String {
    match command.kind {
        TelegramCommandKind::Start | TelegramCommandKind::Help => handle_help(
            workspace_label,
            &session_manager.model_label_for_help(),
            &session_manager.dm_access_label(),
        ),
        TelegramCommandKind::Stop => handle_stop(session_manager, chat_id),
        TelegramCommandKind::Approvals => handle_approvals(session_manager, chat_id),
        TelegramCommandKind::Approve => handle_approve(session_manager, chat_id, command.argument),
        TelegramCommandKind::Deny => handle_deny(session_manager, chat_id, command.argument),
        TelegramCommandKind::Reset => handle_reset(session_manager, chat_id),
    }
}

pub(super) fn handle_help(workspace_label: &str, model_label: &str, dm_access: &str) -> String {
    let mut commands_section = String::from("Commands:\n");
    for spec in TELEGRAM_COMMANDS {
        commands_section.push_str(&format!("{} - {}\n", spec.usage(), spec.description));
    }
    format!(
        "{PRODUCT_NAME}\n\n\
         Workspace: {}\n\
         Model: {}\n\
         DM access: {}\n\
         Mode: private text chats only\n\n\
         {}\
         \nApproval requests include Approve/Deny buttons; slash commands remain available.\n\n\
         Send a plain text message to start a task.",
        workspace_label, model_label, dm_access, commands_section
    )
}

pub(super) fn handle_stop(session_manager: &mut ChatSessionManager, chat_id: i64) -> String {
    if session_manager.stop_chat(chat_id) {
        "Stopping current task...".to_string()
    } else {
        "No task is currently running.".to_string()
    }
}

pub(super) fn handle_approvals(session_manager: &mut ChatSessionManager, chat_id: i64) -> String {
    session_manager.pending_approvals_reply(chat_id)
}

pub(super) fn handle_approve(
    session_manager: &mut ChatSessionManager,
    chat_id: i64,
    argument: &str,
) -> String {
    session_manager.resolve_approval_command(chat_id, argument, true)
}

pub(super) fn handle_deny(
    session_manager: &mut ChatSessionManager,
    chat_id: i64,
    argument: &str,
) -> String {
    session_manager.resolve_approval_command(chat_id, argument, false)
}

pub(super) fn handle_reset(session_manager: &mut ChatSessionManager, chat_id: i64) -> String {
    if session_manager.is_task_running(chat_id) {
        "A task is still running. Send /stop and wait for it to finish before /reset.".to_string()
    } else {
        session_manager.reset_chat(chat_id);
        "Saved chat transcript cleared for this chat. Curated workspace memory was left unchanged."
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_help_includes_key_sections() {
        let reply = handle_help("/workspace/path", "gpt-4o", "unbound");
        assert!(reply.contains("Workspace: /workspace/path"));
        assert!(reply.contains("Model: gpt-4o"));
        assert!(reply.contains("DM access: unbound"));
        for spec in TELEGRAM_COMMANDS {
            let expected = format!("{} - {}", spec.usage(), spec.description);
            assert!(
                reply.contains(&expected),
                "help text missing command declaration `{expected}`"
            );
        }
    }

    #[test]
    fn test_declared_telegram_commands_parse_and_route() {
        use std::path::PathBuf;
        use topagent_core::{ModelRoute, RuntimeOptions};

        let workspace = tempfile::TempDir::new().unwrap();
        let mut session_manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            PathBuf::from(workspace.path()),
            topagent_core::SecretRegistry::new(),
            None,
            None,
            None,
        );

        for spec in TELEGRAM_COMMANDS {
            let text = if spec.arguments.is_empty() {
                spec.command.to_string()
            } else {
                format!("{} apr-1", spec.command)
            };
            let parsed = crate::commands::surface::parse_telegram_command(&text)
                .unwrap_or_else(|| panic!("declared command did not parse: {text}"));
            assert_eq!(parsed.kind, spec.kind);

            let reply =
                handle_parsed_command(parsed, &mut session_manager, 42, "/tmp/topagent-workspace");
            assert!(
                !reply.trim().is_empty(),
                "declared command {} routed to an empty reply",
                spec.command
            );
        }
    }

    #[test]
    fn test_handle_reset_refuses_while_task_is_running() {
        use std::path::PathBuf;
        use topagent_core::{
            ApprovalMailbox, ApprovalMailboxMode, CancellationToken, ModelRoute, RuntimeOptions,
        };

        let workspace = tempfile::TempDir::new().unwrap();
        let mut session_manager = ChatSessionManager::new(
            ModelRoute::openrouter("test-model"),
            "test-model".to_string(),
            "test-key".to_string(),
            RuntimeOptions::default(),
            PathBuf::from(workspace.path()),
            topagent_core::SecretRegistry::new(),
            None,
            None,
            None,
        );
        session_manager.sessions.insert(
            42,
            crate::telegram::session::RunningChatTask {
                cancel_token: CancellationToken::new(),
                progress_callback: None,
                approval_mailbox: ApprovalMailbox::new(ApprovalMailboxMode::Immediate),
                instruction: "test instruction".to_string(),
                started_at: std::time::SystemTime::now(),
            },
        );

        let reply = handle_reset(&mut session_manager, 42);

        assert!(reply.contains("task is still running"));
        assert!(
            session_manager.sessions.contains_key(&42),
            "reset command must not clear a running task"
        );
    }
}

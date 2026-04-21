use crate::commands::surface::{PRODUCT_NAME, TELEGRAM_COMMANDS};
use crate::telegram::session::ChatSessionManager;

pub(super) fn handle_help(
    workspace_label: &str,
    model_label: &str,
    tool_authoring_enabled: bool,
    dm_access: &str,
) -> String {
    let tool_authoring = if tool_authoring_enabled { "on" } else { "off" };
    let mut commands_section = String::from("Commands:\n");
    for spec in TELEGRAM_COMMANDS {
        commands_section.push_str(&format!("{} - {}\n", spec.usage(), spec.description));
    }
    format!(
        "{PRODUCT_NAME}\n\n\
         Workspace: {}\n\
         Model: {}\n\
         Tool authoring: {}\n\
         DM access: {}\n\
         Mode: private text chats only\n\n\
         {}\
         \nApproval requests include Approve/Deny buttons; slash commands remain available.\n\n\
         Send a plain text message to start a task.",
        workspace_label, model_label, tool_authoring, dm_access, commands_section
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
        let reply = handle_help("/workspace/path", "gpt-4o", true, "unbound");
        assert!(reply.contains("Workspace: /workspace/path"));
        assert!(reply.contains("Model: gpt-4o"));
        assert!(reply.contains("Tool authoring: on"));
        assert!(reply.contains("DM access: unbound"));
        for spec in TELEGRAM_COMMANDS {
            assert!(
                reply.contains(spec.command),
                "help text missing command {}",
                spec.command
            );
        }
    }

    #[test]
    fn test_handle_help_tool_authoring_off() {
        let reply = handle_help("/ws", "model", false, "bound");
        assert!(reply.contains("Tool authoring: off"));
    }

    #[test]
    fn test_telegram_commands_match_router() {
        let routed = [
            "/start",
            "/help",
            "/stop",
            "/approvals",
            "/approve",
            "/deny",
            "/reset",
        ];
        for name in &routed {
            let bare = name.trim_start_matches('/');
            let found = TELEGRAM_COMMANDS
                .iter()
                .any(|spec| spec.command.trim_start_matches('/').starts_with(bare));
            assert!(
                found,
                "router handles {} but it is not in TELEGRAM_COMMANDS",
                name
            );
        }
        for spec in TELEGRAM_COMMANDS {
            let bare = spec.command.trim_start_matches('/');
            assert!(
                routed.contains(&format!("/{}", bare).as_str()),
                "TELEGRAM_COMMANDS contains {} but router does not route it",
                spec.command,
            );
        }
    }
}

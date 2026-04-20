use crate::telegram::session::ChatSessionManager;

pub(super) fn handle_help(
    workspace_label: &str,
    model_label: &str,
    tool_authoring_enabled: bool,
    dm_access: &str,
) -> String {
    let tool_authoring = if tool_authoring_enabled { "on" } else { "off" };
    format!(
        "TopAgent\n\n\
         Workspace: {}\n\
         Model: {}\n\
         Tool authoring: {}\n\
         DM access: {}\n\
         Mode: private text chats only\n\n\
         Commands:\n\
         /help - show this message\n\
         /stop - stop the current task\n\
         /approvals - list pending approvals for this chat\n\
         /approve <id> - approve a pending action\n\
         /deny <id> - deny a pending action\n\
         /reset - clear this chat's saved transcript\n\n\
         Approval requests include Approve/Deny buttons; slash commands remain available.\n\n\
         Send a plain text message to start a task.",
        workspace_label, model_label, tool_authoring, dm_access
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
        assert!(reply.contains("/stop"));
        assert!(reply.contains("/approvals"));
        assert!(reply.contains("/approve"));
        assert!(reply.contains("/deny"));
        assert!(reply.contains("/reset"));
    }

    #[test]
    fn test_handle_help_tool_authoring_off() {
        let reply = handle_help("/ws", "model", false, "bound");
        assert!(reply.contains("Tool authoring: off"));
    }
}

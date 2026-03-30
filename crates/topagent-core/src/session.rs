use crate::Message;

#[derive(Debug, Default)]
pub struct Session {
    system_prompt: Option<String>,
    messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt = Some(prompt.to_string());
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> Vec<Message> {
        let mut msgs = Vec::new();
        if let Some(ref sys) = self.system_prompt {
            msgs.push(Message::system(sys));
        }
        msgs.extend(self.messages.clone());
        msgs
    }

    pub fn raw_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Remove the last message if it matches a predicate.
    /// Used to replace ephemeral control messages (e.g., planning redirects)
    /// without accumulating them in history.
    pub fn pop_last_if(&mut self, predicate: impl FnOnce(&Message) -> bool) -> bool {
        if let Some(last) = self.messages.last() {
            if predicate(last) {
                self.messages.pop();
                return true;
            }
        }
        false
    }

    pub fn truncate_history(&mut self, keep_recent: usize) {
        if self.messages.len() <= keep_recent {
            return;
        }

        let dropped_count = self.messages.len() - keep_recent;
        let start = self.messages.len() - keep_recent;
        let recent: Vec<Message> = self.messages.drain(start..).collect();

        self.messages.clear();
        self.messages.push(Message::system(format!(
            "[Previous {} messages truncated due to context length.]\nUse tools to re-read files if you need to recall earlier context.",
            dropped_count
        )));
        self.messages.extend(recent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_add_message() {
        let mut session = Session::new();
        session.add_message(Message::user("hello"));
        assert_eq!(session.messages().len(), 1);
    }

    #[test]
    fn test_session_with_system_prompt() {
        let mut session = Session::new();
        session.set_system_prompt("you are helpful");
        session.add_message(Message::user("hello"));
        let msgs = session.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, crate::Role::System);
    }

    #[test]
    fn test_session_message_count() {
        let mut session = Session::new();
        assert_eq!(session.message_count(), 0);
        session.add_message(Message::user("hello"));
        assert_eq!(session.message_count(), 1);
        session.add_message(Message::assistant("hi"));
        assert_eq!(session.message_count(), 2);
    }

    #[test]
    fn test_session_truncate_history_keeps_recent_messages() {
        let mut session = Session::new();
        session.set_system_prompt("base prompt");
        for i in 0..20 {
            session.add_message(Message::user(format!("message {}", i)));
        }
        assert_eq!(session.message_count(), 20);

        session.truncate_history(5);

        assert_eq!(session.message_count(), 6);
        let msgs = session.messages();
        assert_eq!(msgs.len(), 7);
        assert_eq!(msgs[0].role, crate::Role::System);
        assert!(msgs[1].as_text().unwrap().contains("truncated"));
        assert!(msgs[1].as_text().unwrap().contains("15"));
        assert!(msgs[2].as_text().unwrap().contains("message 15"));
    }

    #[test]
    fn test_session_truncate_history_does_nothing_when_small() {
        let mut session = Session::new();
        session.add_message(Message::user("hello"));
        session.add_message(Message::user("world"));
        assert_eq!(session.message_count(), 2);

        session.truncate_history(10);

        assert_eq!(session.message_count(), 2);
    }

    #[test]
    fn test_session_pop_last_if_matches() {
        let mut session = Session::new();
        session.add_message(Message::user("keep"));
        session.add_message(Message::user("remove me"));
        assert_eq!(session.message_count(), 2);

        let popped = session.pop_last_if(|m| m.as_text() == Some("remove me"));
        assert!(popped);
        assert_eq!(session.message_count(), 1);
        assert_eq!(session.messages()[0].as_text(), Some("keep"));
    }

    #[test]
    fn test_session_pop_last_if_no_match() {
        let mut session = Session::new();
        session.add_message(Message::user("keep"));
        let popped = session.pop_last_if(|m| m.as_text() == Some("nope"));
        assert!(!popped);
        assert_eq!(session.message_count(), 1);
    }

    #[test]
    fn test_session_truncate_history_preserves_order() {
        let mut session = Session::new();
        session.set_system_prompt("base");
        for i in 0..10 {
            session.add_message(Message::user(format!("msg{}", i)));
        }
        assert_eq!(session.message_count(), 10);

        session.truncate_history(3);

        assert_eq!(session.message_count(), 4);
        let msgs: Vec<_> = session
            .messages()
            .iter()
            .map(|m| m.as_text().unwrap().to_string())
            .collect();
        assert_eq!(msgs[0], "base");
        assert!(msgs[1].contains("truncated"));
        assert!(msgs[2].contains("msg7"));
        assert!(msgs[3].contains("msg8"));
        assert!(msgs[4].contains("msg9"));
    }

    #[test]
    fn test_session_raw_messages_excludes_system_prompt() {
        let mut session = Session::new();
        session.set_system_prompt("base prompt");
        session.add_message(Message::user("hello"));
        session.add_message(Message::assistant("hi"));

        let raw = session.raw_messages();

        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].as_text(), Some("hello"));
        assert_eq!(raw[1].as_text(), Some("hi"));
    }

    #[test]
    fn test_session_replace_messages_restores_history_without_system_prompt() {
        let mut session = Session::new();
        session.set_system_prompt("base prompt");
        session.replace_messages(vec![
            Message::user("remember"),
            Message::assistant("stored"),
        ]);

        let messages = session.messages();

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].as_text(), Some("base prompt"));
        assert_eq!(messages[1].as_text(), Some("remember"));
        assert_eq!(messages[2].as_text(), Some("stored"));
    }
}

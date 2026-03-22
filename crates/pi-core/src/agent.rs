use crate::context::ExecutionContext;
use crate::prompt;
use crate::session::Session;
use crate::tools::Tool;
use crate::{Error, Message, Provider, ProviderResponse, Result};
use std::collections::HashMap;

pub struct Agent {
    session: Session,
    provider: Box<dyn Provider>,
    tools: HashMap<String, Box<dyn Tool>>,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Self {
        let tools: HashMap<_, _> = tools
            .into_iter()
            .map(|t| (t.name().to_string(), t))
            .collect();
        Self {
            session: Session::new(),
            provider,
            tools,
        }
    }

    pub fn run(&mut self, ctx: &ExecutionContext, instruction: &str) -> Result<String> {
        self.session.add_message(Message::user(instruction));
        let specs: Vec<_> = self.tools.values().map(|t| t.spec()).collect();
        let system_prompt = prompt::build_system_prompt(&specs);
        self.session.set_system_prompt(&system_prompt);
        loop {
            let response = self.provider.complete(&self.session.messages())?;
            match response {
                ProviderResponse::Message(msg) => {
                    let text = msg.as_text().map(|s| s.to_string());
                    if let Some(text) = text {
                        if text.is_empty() {
                            return Err(Error::Provider("empty response".into()));
                        }
                        self.session.add_message(msg);
                        return Ok(text);
                    }
                    self.session.add_message(msg);
                }
                ProviderResponse::ToolCall { id, name, args } => {
                    let tool = self
                        .tools
                        .get(&name)
                        .ok_or_else(|| Error::ToolFailed(format!("unknown tool: {}", name)))?;
                    let result = tool.execute(args.clone(), ctx)?;
                    self.session
                        .add_message(Message::tool_request(id.clone(), name, args));
                    self.session.add_message(Message::tool_result(id, result));
                }
                ProviderResponse::RequiresInput => {
                    return Err(Error::Session(
                        "provider requires input, but session is complete".into(),
                    ));
                }
            }
        }
    }
}

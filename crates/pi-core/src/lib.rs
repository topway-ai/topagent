pub mod agent;
pub mod commands;
pub mod context;
pub mod error;
pub mod external;
pub mod file_util;
pub mod hooks;
pub mod message;
pub mod model;
pub mod openrouter;
pub mod plan;
pub mod project;
pub mod prompt;
pub mod provider;
pub mod provider_factory;
pub mod runtime;
pub mod session;
pub mod tool_spec;
pub mod tools;

pub use agent::Agent;
pub use context::ExecutionContext;
pub use error::{Error, Result};
pub use external::{ExternalTool, ExternalToolRegistry};
pub use hooks::{HookRegistry, ToolHooks};
pub use message::{Content, Message, Role};
pub use model::{ModelRoute, ProviderId, RoutingPolicy, TaskCategory};
pub use openrouter::OpenRouterProvider;
pub use plan::{Plan, TodoItem, TodoStatus};
pub use project::{
    get_project_instructions_or_error, load_project_instructions, ProjectInstructionResult,
};
pub use provider::{Provider, ProviderResponse, ScriptedProvider, ToolCallEntry};
pub use provider_factory::create_provider;
pub use runtime::RuntimeOptions;
pub use session::Session;
pub use tool_spec::ToolSpec;

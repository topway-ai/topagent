use crate::approval::ApprovalRequest;
use crate::capability::CapabilityError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("tool execution failed: {0}")]
    ToolFailed(String),

    #[error("edit failed: {0}")]
    EditFailed(String),

    #[error("read failed: {0}")]
    ReadFailed(String),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("provider request failed: {0}")]
    ProviderRequestFailed(String),

    #[error("provider response parse failed: {0}")]
    ProviderParseFailed(String),

    #[error("provider unsupported: {0}")]
    ProviderUnsupported(String),

    #[error("provider retry exhausted: {0}")]
    ProviderRetryExhausted(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("max steps reached: {0}")]
    MaxStepsReached(String),

    #[error("stopped: {0}")]
    Stopped(String),

    #[error("approval required: {0}")]
    ApprovalRequired(Box<ApprovalRequest>),

    #[error("capability error: {0}")]
    Capability(Box<CapabilityError>),

    #[error("project instruction error: {0}")]
    ProjectInstruction(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

mod audit;
mod decision;
mod grant;
mod profile;
mod risk;

pub use audit::{AuditEvent, CapabilityAuditLog, CapabilityAuditRecord};
pub use decision::{
    CapabilityApprovalDraft, CapabilityApprovalRequest, CapabilityDecision,
    CapabilityDecisionDetail, CapabilityError, CapabilityManager, CapabilityRequest,
};
pub use grant::{AccessMode, CapabilityGrant, GrantScope};
pub use profile::{AccessConfig, AccessConfigDocument, CapabilityKind, CapabilityProfile};
pub use risk::{
    assess_computer_action, assess_shell_command, is_secret_target, redact_sensitive_target,
    RiskLevel, ShellAssessment,
};

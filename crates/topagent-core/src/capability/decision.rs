use crate::capability::{
    AccessConfig, AccessConfigDocument, AccessMode, AuditEvent, CapabilityAuditLog,
    CapabilityAuditRecord, CapabilityGrant, CapabilityKind, CapabilityProfile, GrantScope,
    RiskLevel,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub kind: CapabilityKind,
    pub target: String,
    pub mode: AccessMode,
    pub risk: RiskLevel,
    pub reason: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
}

impl CapabilityRequest {
    pub fn new(
        kind: CapabilityKind,
        target: impl Into<String>,
        mode: AccessMode,
        risk: RiskLevel,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            target: target.into(),
            mode,
            risk,
            reason: reason.into(),
            task_id: None,
            session_id: None,
        }
    }

    pub fn with_task_id(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }

    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDecisionDetail {
    pub kind: CapabilityKind,
    pub target: String,
    pub mode: AccessMode,
    pub risk: RiskLevel,
    pub reason: String,
    pub profile: CapabilityProfile,
    pub approval_possible: bool,
    pub suggested_scopes: Vec<GrantScope>,
}

impl CapabilityDecisionDetail {
    fn from_request(
        request: &CapabilityRequest,
        profile: CapabilityProfile,
        approval_possible: bool,
        suggested_scopes: Vec<GrantScope>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind: request.kind,
            target: request.target.clone(),
            mode: request.mode,
            risk: request.risk,
            reason: reason.into(),
            profile,
            approval_possible,
            suggested_scopes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityApprovalDraft {
    pub detail: CapabilityDecisionDetail,
    pub approval_options: Vec<GrantScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityApprovalRequest {
    pub request_id: String,
    pub detail: CapabilityDecisionDetail,
    pub approval_options: Vec<GrantScope>,
}

impl CapabilityApprovalRequest {
    pub fn from_draft(request_id: impl Into<String>, draft: CapabilityApprovalDraft) -> Self {
        Self {
            request_id: request_id.into(),
            detail: draft.detail,
            approval_options: draft.approval_options,
        }
    }

    pub fn to_grant(&self, scope: GrantScope, persisted: bool) -> CapabilityGrant {
        let target = if scope == GrantScope::ThisPath {
            self.detail.target.clone()
        } else {
            self.detail.target.clone()
        };
        CapabilityGrant::new(
            self.detail.kind,
            target,
            self.detail.mode,
            scope,
            self.detail.reason.clone(),
        )
        .with_id(format!("grant-{}", self.request_id))
        .persisted(persisted)
    }

    pub fn render_access_request(&self) -> String {
        render_access_request(&self.detail, &self.approval_options, Some(&self.request_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityDecision {
    Allow(CapabilityDecisionDetail),
    Deny(CapabilityDecisionDetail),
    NeedsApproval(CapabilityApprovalDraft),
}

impl CapabilityDecision {
    pub fn detail(&self) -> &CapabilityDecisionDetail {
        match self {
            Self::Allow(detail) | Self::Deny(detail) => detail,
            Self::NeedsApproval(request) => &request.detail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityError {
    NeedsApproval {
        request_id: String,
        kind: CapabilityKind,
        target: String,
        mode: AccessMode,
        risk: RiskLevel,
        reason: String,
        approval_options: Vec<GrantScope>,
    },
    Denied {
        kind: CapabilityKind,
        target: String,
        mode: AccessMode,
        risk: RiskLevel,
        reason: String,
    },
}

impl CapabilityError {
    pub fn from_approval_request(request: &CapabilityApprovalRequest) -> Self {
        Self::NeedsApproval {
            request_id: request.request_id.clone(),
            kind: request.detail.kind,
            target: request.detail.target.clone(),
            mode: request.detail.mode,
            risk: request.detail.risk,
            reason: request.detail.reason.clone(),
            approval_options: request.approval_options.clone(),
        }
    }

    fn from_decision(decision: CapabilityDecision) -> Self {
        match decision {
            CapabilityDecision::Allow(detail) => Self::Denied {
                kind: detail.kind,
                target: detail.target,
                mode: detail.mode,
                risk: detail.risk,
                reason: "capability unexpectedly denied after allow decision".to_string(),
            },
            CapabilityDecision::Deny(detail) => Self::Denied {
                kind: detail.kind,
                target: detail.target,
                mode: detail.mode,
                risk: detail.risk,
                reason: detail.reason,
            },
            CapabilityDecision::NeedsApproval(draft) => Self::NeedsApproval {
                request_id: "unavailable".to_string(),
                kind: draft.detail.kind,
                target: draft.detail.target,
                mode: draft.detail.mode,
                risk: draft.detail.risk,
                reason: draft.detail.reason,
                approval_options: draft.approval_options,
            },
        }
    }
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsApproval {
                request_id,
                kind,
                target,
                mode,
                risk,
                reason,
                approval_options,
            } => write!(
                f,
                "access approval required ({request_id}): {kind} {} access to {target}; risk: {risk}; reason: {reason}; scopes: {}",
                mode.label(),
                approval_options
                    .iter()
                    .map(|scope| scope.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Denied {
                kind,
                target,
                mode,
                risk,
                reason,
            } => write!(
                f,
                "access denied: {kind} {} access to {target}; risk: {risk}; reason: {reason}",
                mode.label()
            ),
        }
    }
}

#[derive(Debug)]
struct CapabilityState {
    config: AccessConfig,
    grants: Vec<CapabilityGrant>,
    actor: String,
    source: String,
    store_path: Option<PathBuf>,
    audit_log: Option<CapabilityAuditLog>,
}

#[derive(Debug, Clone)]
pub struct CapabilityManager {
    inner: Arc<Mutex<CapabilityState>>,
}

impl Default for CapabilityManager {
    fn default() -> Self {
        Self::new(AccessConfig::default(), Vec::new(), "runtime", "default")
    }
}

impl CapabilityManager {
    pub fn new(
        config: AccessConfig,
        grants: Vec<CapabilityGrant>,
        actor: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CapabilityState {
                config,
                grants,
                actor: actor.into(),
                source: source.into(),
                store_path: None,
                audit_log: None,
            })),
        }
    }

    pub fn with_store_path(self, path: PathBuf) -> Self {
        self.inner.lock().unwrap().store_path = Some(path);
        self
    }

    pub fn with_audit_log(self, audit_log: CapabilityAuditLog) -> Self {
        self.inner.lock().unwrap().audit_log = Some(audit_log);
        self
    }

    pub fn config(&self) -> AccessConfig {
        self.inner.lock().unwrap().config.clone()
    }

    pub fn grants(&self) -> Vec<CapabilityGrant> {
        self.inner.lock().unwrap().grants.clone()
    }

    pub fn set_profile(&self, profile: CapabilityProfile, reason: impl Into<String>) {
        let reason = reason.into();
        {
            let mut state = self.inner.lock().unwrap();
            state.config.set_profile_defaults(profile);
            self.audit_locked(
                &state,
                AuditEvent::ProfileChanged,
                None,
                "profile_changed",
                reason.clone(),
            );
        }
        self.persist_grants_and_config();
    }

    pub fn add_grant(&self, grant: CapabilityGrant) {
        {
            let mut state = self.inner.lock().unwrap();
            self.audit_locked(
                &state,
                AuditEvent::GrantCreated,
                Some(&CapabilityRequest::new(
                    grant.kind,
                    grant.target.clone(),
                    grant.mode,
                    RiskLevel::Moderate,
                    grant.reason.clone(),
                )),
                "grant_created",
                format!("{} grant {}", grant.scope.as_str(), grant.id),
            );
            state.grants.push(grant);
        }
        self.persist_grants_and_config();
    }

    pub fn revoke_grants_for_target(&self, target: &str) -> usize {
        let removed = {
            let mut state = self.inner.lock().unwrap();
            let before = state.grants.len();
            state
                .grants
                .retain(|grant| grant.target != target && grant.id != target);
            let removed = before.saturating_sub(state.grants.len());
            if removed > 0 {
                self.audit_locked(
                    &state,
                    AuditEvent::GrantRevoked,
                    None,
                    "grant_revoked",
                    format!("revoked grants matching {target}"),
                );
            }
            removed
        };
        if removed > 0 {
            self.persist_grants_and_config();
        }
        removed
    }

    pub fn lockdown(&self) {
        {
            let mut state = self.inner.lock().unwrap();
            state.config.lockdown();
            state.grants.clear();
            self.audit_locked(
                &state,
                AuditEvent::LockdownActivated,
                None,
                "lockdown",
                "workspace profile restored; grants cleared".to_string(),
            );
        }
        self.persist_grants_and_config();
    }

    pub fn clear_task_temporary_grants(&self, task_id: &str) -> usize {
        let removed = {
            let mut state = self.inner.lock().unwrap();
            let before = state.grants.len();
            state.grants.retain(|grant| {
                !(grant.is_temporary() && grant.task_id.as_deref() == Some(task_id))
            });
            let removed = before.saturating_sub(state.grants.len());
            if removed > 0 {
                self.audit_locked(
                    &state,
                    AuditEvent::GrantExpired,
                    None,
                    "expired",
                    format!("cleared temporary grants for task {task_id}"),
                );
            }
            removed
        };
        if removed > 0 {
            self.persist_grants_and_config();
        }
        removed
    }

    pub fn check(&self, request: &CapabilityRequest, workspace_root: &Path) -> CapabilityDecision {
        let state = self.inner.lock().unwrap();
        if let Some(grant) = state
            .grants
            .iter()
            .find(|grant| grant.matches_request(request))
        {
            return CapabilityDecision::Allow(CapabilityDecisionDetail::from_request(
                request,
                state.config.profile,
                false,
                Vec::new(),
                format!("allowed by scoped grant {}", grant.id),
            ));
        }

        decision_from_config(&state.config, request, workspace_root)
    }

    pub fn authorize(
        &self,
        request: &CapabilityRequest,
        workspace_root: &Path,
    ) -> Result<(), CapabilityError> {
        let mut persist = false;
        let mut allowed = false;
        {
            let mut state = self.inner.lock().unwrap();
            if let Some(index) = state
                .grants
                .iter()
                .position(|grant| grant.matches_request(request))
            {
                let mut grant = state.grants[index].clone();
                grant.bind_scope_if_needed(request);
                grant.consume();
                state.grants[index] = grant.clone();
                self.audit_locked(
                    &state,
                    AuditEvent::GrantUsed,
                    Some(request),
                    "allow",
                    format!("used scoped grant {}", grant.id),
                );
                if grant.is_expired() {
                    self.audit_locked(
                        &state,
                        AuditEvent::GrantExpired,
                        Some(request),
                        "expired",
                        format!("grant {} was consumed", grant.id),
                    );
                }
                state.grants.retain(|grant| !grant.is_expired());
                persist = grant.persisted || state.grants.iter().any(|grant| grant.persisted);
                allowed = true;
            } else {
                match decision_from_config(&state.config, request, workspace_root) {
                    CapabilityDecision::Allow(detail) => {
                        if detail.risk.is_high_impact() {
                            self.audit_locked(
                                &state,
                                AuditEvent::HighRiskAllowed,
                                Some(request),
                                "allow",
                                detail.reason,
                            );
                        }
                        allowed = true;
                    }
                    CapabilityDecision::Deny(detail) => {
                        if detail.risk.is_high_impact() {
                            self.audit_locked(
                                &state,
                                AuditEvent::HighRiskBlocked,
                                Some(request),
                                "deny",
                                detail.reason.clone(),
                            );
                        }
                        Err(CapabilityError::from_decision(CapabilityDecision::Deny(
                            detail,
                        )))?
                    }
                    CapabilityDecision::NeedsApproval(draft) => {
                        if draft.detail.risk.is_high_impact() {
                            self.audit_locked(
                                &state,
                                AuditEvent::HighRiskBlocked,
                                Some(request),
                                "needs_approval",
                                draft.detail.reason.clone(),
                            );
                        }
                        Err(CapabilityError::from_decision(
                            CapabilityDecision::NeedsApproval(draft),
                        ))?
                    }
                }
            }
        }
        if persist {
            self.persist_grants_and_config();
        }
        if allowed {
            Ok(())
        } else {
            Err(CapabilityError::Denied {
                kind: request.kind,
                target: request.target.clone(),
                mode: request.mode,
                risk: request.risk,
                reason: "capability request was not allowed".to_string(),
            })
        }
    }

    pub fn record_approval_requested(&self, request: &CapabilityRequest) {
        let state = self.inner.lock().unwrap();
        self.audit_locked(
            &state,
            AuditEvent::ApprovalRequested,
            Some(request),
            "needs_approval",
            request.reason.clone(),
        );
    }

    pub fn record_approval_result(
        &self,
        request: &CapabilityRequest,
        accepted: bool,
        reason: impl Into<String>,
    ) {
        let state = self.inner.lock().unwrap();
        self.audit_locked(
            &state,
            if accepted {
                AuditEvent::ApprovalAccepted
            } else {
                AuditEvent::ApprovalDenied
            },
            Some(request),
            if accepted { "approved" } else { "denied" },
            reason.into(),
        );
    }

    fn persist_grants_and_config(&self) {
        let (path, document) = {
            let state = self.inner.lock().unwrap();
            let Some(path) = state.store_path.clone() else {
                return;
            };
            (
                path,
                AccessConfigDocument {
                    access: state.config.clone(),
                    grants: state
                        .grants
                        .iter()
                        .filter(|grant| grant.persisted && !grant.is_expired())
                        .cloned()
                        .collect(),
                },
            )
        };
        let _ = document.save_to_path(&path);
    }

    fn audit_locked(
        &self,
        state: &CapabilityState,
        event: AuditEvent,
        request: Option<&CapabilityRequest>,
        decision: impl Into<String>,
        reason: impl Into<String>,
    ) {
        let Some(audit_log) = &state.audit_log else {
            return;
        };
        let record = CapabilityAuditRecord::new(
            event,
            state.actor.clone(),
            state.source.clone(),
            request.and_then(|request| request.task_id.clone()),
            request.and_then(|request| request.session_id.clone()),
            request.map(|request| request.kind),
            request.map(|request| request.target.clone()),
            request.map(|request| request.mode),
            request.map(|request| request.risk),
            state.config.profile,
            decision,
            reason,
        );
        let _ = audit_log.append(&record);
    }
}

fn decision_from_config(
    config: &AccessConfig,
    request: &CapabilityRequest,
    workspace_root: &Path,
) -> CapabilityDecision {
    let approval_scopes = suggested_scopes(request);
    let profile = config.profile;

    if request.kind == CapabilityKind::SecretRead {
        return needs_approval_or_deny(
            config.require_approval_for_secret_paths,
            request,
            profile,
            approval_scopes,
            "secret-bearing paths require explicit approval",
        );
    }

    if request.risk.is_high_impact() {
        let requires = match request.kind {
            CapabilityKind::ExternalSend => config.require_approval_for_external_send,
            CapabilityKind::PackageManager => config.require_approval_for_global_package_install,
            CapabilityKind::Git => config.require_approval_for_git_push,
            CapabilityKind::SystemService | CapabilityKind::Shell => {
                config.require_approval_for_sudo
            }
            CapabilityKind::Filesystem => config.require_approval_for_destructive,
            CapabilityKind::ComputerUse => true,
            _ => true,
        };
        if requires {
            return CapabilityDecision::NeedsApproval(CapabilityApprovalDraft {
                detail: CapabilityDecisionDetail::from_request(
                    request,
                    profile,
                    true,
                    approval_scopes.clone(),
                    request.reason.clone(),
                ),
                approval_options: approval_scopes,
            });
        }
    }

    match request.kind {
        CapabilityKind::Filesystem => filesystem_decision(config, request, workspace_root),
        CapabilityKind::Network => default_decision(
            config.network_default
                && matches!(
                    profile,
                    CapabilityProfile::Developer
                        | CapabilityProfile::Computer
                        | CapabilityProfile::Full
                ),
            request,
            profile,
            approval_scopes,
            "network access is disabled for this profile",
        ),
        CapabilityKind::WebSearch => default_decision(
            config.web_search_default
                && matches!(
                    profile,
                    CapabilityProfile::Developer
                        | CapabilityProfile::Computer
                        | CapabilityProfile::Full
                ),
            request,
            profile,
            approval_scopes,
            "web search is disabled for this profile",
        ),
        CapabilityKind::ComputerUse => default_decision(
            (config.computer_use_default
                && matches!(profile, CapabilityProfile::Computer | CapabilityProfile::Full))
                || matches!(profile, CapabilityProfile::Computer | CapabilityProfile::Full),
            request,
            profile,
            approval_scopes,
            "computer_use is disabled unless the computer/full profile or an explicit grant allows it",
        ),
        CapabilityKind::Shell
        | CapabilityKind::Git
        | CapabilityKind::PackageManager
        | CapabilityKind::MemoryWrite => CapabilityDecision::Allow(
            CapabilityDecisionDetail::from_request(
                request,
                profile,
                false,
                Vec::new(),
                request.reason.clone(),
            ),
        ),
        CapabilityKind::ExternalSend | CapabilityKind::SystemService => {
            CapabilityDecision::NeedsApproval(CapabilityApprovalDraft {
                detail: CapabilityDecisionDetail::from_request(
                    request,
                    profile,
                    true,
                    approval_scopes.clone(),
                    request.reason.clone(),
                ),
                approval_options: approval_scopes,
            })
        }
        CapabilityKind::SecretRead => unreachable!("handled above"),
    }
}

fn filesystem_decision(
    config: &AccessConfig,
    request: &CapabilityRequest,
    workspace_root: &Path,
) -> CapabilityDecision {
    let profile = config.profile;
    let approval_scopes = suggested_scopes(request);
    if path_in_workspace(&request.target, workspace_root) {
        let allowed = match request.mode {
            AccessMode::Read => true,
            AccessMode::Write | AccessMode::ReadWrite => config.allow_workspace_write,
            AccessMode::Execute => true,
        };
        return default_decision(
            allowed,
            request,
            profile,
            approval_scopes,
            "workspace writes are disabled by access config",
        );
    }

    if path_in_home(&request.target) {
        let allowed = match request.mode {
            AccessMode::Read => config.allow_home_read || profile == CapabilityProfile::Full,
            AccessMode::Write | AccessMode::ReadWrite => {
                config.allow_home_write || profile == CapabilityProfile::Full
            }
            AccessMode::Execute => profile == CapabilityProfile::Full,
        };
        return default_decision(
            allowed,
            request,
            profile,
            approval_scopes,
            "home-directory access is disabled for this profile",
        );
    }

    default_decision(
        profile == CapabilityProfile::Full,
        request,
        profile,
        approval_scopes,
        "path is outside the configured workspace",
    )
}

fn default_decision(
    allowed: bool,
    request: &CapabilityRequest,
    profile: CapabilityProfile,
    approval_scopes: Vec<GrantScope>,
    deny_reason: &str,
) -> CapabilityDecision {
    let reason = if request.reason.trim().is_empty() {
        deny_reason.to_string()
    } else {
        format!("{}; policy: {}", request.reason, deny_reason)
    };
    if allowed {
        CapabilityDecision::Allow(CapabilityDecisionDetail::from_request(
            request,
            profile,
            false,
            Vec::new(),
            request.reason.clone(),
        ))
    } else {
        CapabilityDecision::NeedsApproval(CapabilityApprovalDraft {
            detail: CapabilityDecisionDetail::from_request(
                request,
                profile,
                true,
                approval_scopes.clone(),
                reason,
            ),
            approval_options: approval_scopes,
        })
    }
}

fn needs_approval_or_deny(
    approval_possible: bool,
    request: &CapabilityRequest,
    profile: CapabilityProfile,
    approval_scopes: Vec<GrantScope>,
    reason: &str,
) -> CapabilityDecision {
    let rendered_reason = if request.reason.trim().is_empty() {
        reason.to_string()
    } else {
        format!("{}; policy: {}", request.reason, reason)
    };
    if approval_possible {
        CapabilityDecision::NeedsApproval(CapabilityApprovalDraft {
            detail: CapabilityDecisionDetail::from_request(
                request,
                profile,
                true,
                approval_scopes.clone(),
                rendered_reason,
            ),
            approval_options: approval_scopes,
        })
    } else {
        CapabilityDecision::Deny(CapabilityDecisionDetail::from_request(
            request,
            profile,
            false,
            Vec::new(),
            rendered_reason,
        ))
    }
}

fn suggested_scopes(request: &CapabilityRequest) -> Vec<GrantScope> {
    match request.kind {
        CapabilityKind::Filesystem | CapabilityKind::SecretRead => {
            vec![GrantScope::Once, GrantScope::ThisTask, GrantScope::ThisPath]
        }
        _ => vec![GrantScope::Once, GrantScope::ThisTask, GrantScope::Session],
    }
}

fn path_in_workspace(target: &str, workspace_root: &Path) -> bool {
    let workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let target = PathBuf::from(target);
    let target = target.canonicalize().unwrap_or(target);
    target.starts_with(workspace)
}

fn path_in_home(target: &str) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    PathBuf::from(target).starts_with(home)
}

pub fn render_access_request(
    detail: &CapabilityDecisionDetail,
    options: &[GrantScope],
    request_id: Option<&str>,
) -> String {
    let target = crate::capability::redact_sensitive_target(&detail.target);
    let mut rendered = format!(
        "TopAgent needs {} access to {} to continue.\n\nReason: {}\nRisk: {}\nProfile: {}",
        detail.mode.label(),
        target,
        detail.reason,
        detail.risk,
        detail.profile
    );
    if let Some(request_id) = request_id {
        rendered.push_str(&format!("\nRequest: {request_id}"));
    }
    rendered.push_str("\nScope options:");
    for scope in options {
        rendered.push_str(&format!("\n- {}", scope.as_str()));
    }
    rendered
}

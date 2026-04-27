use crate::approval::ApprovalMailbox;
use crate::approval::{ApprovalCheck, ApprovalRequestDraft, ApprovalTriggerKind};
use crate::capability::{
    redact_sensitive_target, AccessMode, CapabilityDecision, CapabilityError, CapabilityKind,
    CapabilityManager, CapabilityRequest, GrantScope, RiskLevel,
};
use crate::provenance::RunTrustContext;
use crate::run_snapshot::WorkspaceRunSnapshotStore;
use crate::secrets::SecretRegistry;
use crate::{cancel::CancellationToken, runtime::RuntimeOptions, Error};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub workspace_root: PathBuf,
    cancel_token: Option<CancellationToken>,
    secrets: SecretRegistry,
    memory_context: Option<String>,
    operator_context: Option<String>,
    run_trust_context: RunTrustContext,
    approval_mailbox: Option<ApprovalMailbox>,
    run_snapshot_store: Option<WorkspaceRunSnapshotStore>,
    capability_manager: Option<CapabilityManager>,
    task_id: Option<String>,
    session_id: Option<String>,
}

impl ExecutionContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            cancel_token: None,
            secrets: SecretRegistry::new(),
            memory_context: None,
            operator_context: None,
            run_trust_context: RunTrustContext::default(),
            approval_mailbox: None,
            run_snapshot_store: None,
            capability_manager: None,
            task_id: None,
            session_id: None,
        }
    }

    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        self.cancel_token = Some(cancel_token);
        self
    }

    pub fn with_secrets(mut self, secrets: SecretRegistry) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_memory_context(mut self, memory_context: impl Into<String>) -> Self {
        let memory_context = memory_context.into();
        self.memory_context = if memory_context.trim().is_empty() {
            None
        } else {
            Some(memory_context)
        };
        self
    }

    pub fn with_operator_context(mut self, operator_context: impl Into<String>) -> Self {
        let operator_context = operator_context.into();
        self.operator_context = if operator_context.trim().is_empty() {
            None
        } else {
            Some(operator_context)
        };
        self
    }

    pub fn with_approval_mailbox(mut self, approval_mailbox: ApprovalMailbox) -> Self {
        self.approval_mailbox = Some(approval_mailbox);
        self
    }

    pub fn with_run_trust_context(mut self, run_trust_context: RunTrustContext) -> Self {
        self.run_trust_context = run_trust_context;
        self
    }

    pub fn with_workspace_run_snapshot_store(
        mut self,
        run_snapshot_store: WorkspaceRunSnapshotStore,
    ) -> Self {
        self.run_snapshot_store = Some(run_snapshot_store);
        self
    }

    pub fn with_capability_manager(mut self, capability_manager: CapabilityManager) -> Self {
        self.capability_manager = Some(capability_manager);
        self
    }

    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn secrets(&self) -> &SecretRegistry {
        &self.secrets
    }

    pub fn memory_context(&self) -> Option<&str> {
        self.memory_context.as_deref()
    }

    pub fn operator_context(&self) -> Option<&str> {
        self.operator_context.as_deref()
    }

    pub fn approval_mailbox(&self) -> Option<&ApprovalMailbox> {
        self.approval_mailbox.as_ref()
    }

    pub fn run_trust_context(&self) -> &RunTrustContext {
        &self.run_trust_context
    }

    pub fn run_snapshot_store(&self) -> Option<&WorkspaceRunSnapshotStore> {
        self.run_snapshot_store.as_ref()
    }

    pub fn capability_manager(&self) -> Option<&CapabilityManager> {
        self.capability_manager.as_ref()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    pub fn cancel_token(&self) -> Option<&CancellationToken> {
        self.cancel_token.as_ref()
    }

    pub fn resolve_path(&self, relative_path: &str) -> Result<PathBuf, super::Error> {
        let relative_path = Path::new(relative_path);

        if relative_path.is_absolute() {
            return Err(super::Error::InvalidInput(
                "absolute paths not allowed".into(),
            ));
        }

        for component in relative_path.components() {
            match component {
                Component::ParentDir => {
                    return Err(super::Error::InvalidInput(
                        "path traversal not allowed".into(),
                    ));
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(super::Error::InvalidInput(
                        "path contains root or prefix component".into(),
                    ));
                }
                Component::Normal(_) | Component::CurDir => {}
            }
        }

        let target = self.workspace_root.join(relative_path);

        let canonical_workspace = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        if let Ok(canonical_target) = target.canonicalize() {
            if !canonical_target.starts_with(&canonical_workspace) {
                return Err(super::Error::InvalidInput("path escapes workspace".into()));
            }
        } else if !target.starts_with(&canonical_workspace) {
            return Err(super::Error::InvalidInput("path escapes workspace".into()));
        }

        Ok(target)
    }

    pub fn resolve_path_for_access(
        &self,
        path: &str,
        mode: AccessMode,
        reason: impl Into<String>,
    ) -> Result<PathBuf, super::Error> {
        if self.capability_manager.is_none() {
            return self.resolve_path(path);
        }

        let target = self.resolve_access_target(path)?;
        let target_label = target.display().to_string();
        let kind = if crate::capability::is_secret_target(&target_label) {
            CapabilityKind::SecretRead
        } else {
            CapabilityKind::Filesystem
        };
        let risk = if kind == CapabilityKind::SecretRead {
            RiskLevel::Critical
        } else if path_in_workspace(&target, &self.workspace_root) {
            RiskLevel::Safe
        } else {
            RiskLevel::Moderate
        };
        self.authorize_capability(CapabilityRequest::new(
            kind,
            target_label,
            mode,
            risk,
            reason,
        ))?;
        Ok(target)
    }

    pub fn authorize_capability(&self, request: CapabilityRequest) -> Result<(), super::Error> {
        let request = request
            .with_task_id(self.task_id.clone())
            .with_session_id(self.session_id.clone());
        let Some(manager) = &self.capability_manager else {
            return Ok(());
        };

        match manager.check(&request, &self.workspace_root) {
            CapabilityDecision::Allow(_) => {
                manager
                    .authorize(&request, &self.workspace_root)
                    .map_err(|err| Error::Capability(Box::new(err)))?;
                Ok(())
            }
            CapabilityDecision::Deny(detail) => {
                Err(Error::Capability(Box::new(CapabilityError::Denied {
                    kind: detail.kind,
                    target: detail.target,
                    mode: detail.mode,
                    risk: detail.risk,
                    reason: detail.reason,
                })))
            }
            CapabilityDecision::NeedsApproval(draft) => {
                manager.record_approval_requested(&request);
                let Some(mailbox) = &self.approval_mailbox else {
                    return Err(Error::Capability(Box::new(
                        CapabilityError::NeedsApproval {
                            request_id: "unavailable".to_string(),
                            kind: draft.detail.kind,
                            target: draft.detail.target,
                            mode: draft.detail.mode,
                            risk: draft.detail.risk,
                            reason: draft.detail.reason,
                            approval_options: draft.approval_options,
                        },
                    )));
                };

                let short_summary = format!(
                    "{} {} access to {}",
                    draft.detail.kind,
                    draft.detail.mode.label(),
                    redact_sensitive_target(&draft.detail.target)
                );
                let approval = ApprovalRequestDraft {
                    action_kind: ApprovalTriggerKind::CapabilityAccess,
                    short_summary,
                    exact_action: draft.detail.target.clone(),
                    reason: draft.detail.reason.clone(),
                    scope_of_impact: format!(
                        "{} {} access under the {} profile",
                        draft.detail.kind,
                        draft.detail.mode.label(),
                        draft.detail.profile
                    ),
                    expected_effect: "Creates a scoped grant if approved, then retries the blocked operation."
                        .to_string(),
                    rollback_hint: Some(
                        "Use `topagent access revoke <target>` or `topagent access lockdown` to remove grants."
                            .to_string(),
                    ),
                    capability: Some(draft),
                };

                match mailbox.request_decision(approval, self.cancel_token()) {
                    ApprovalCheck::Approved(entry) => {
                        let Some(capability) = &entry.request.capability else {
                            return Ok(());
                        };
                        let scope = entry
                            .capability_scope
                            .or_else(|| capability.approval_options.first().copied())
                            .unwrap_or(GrantScope::Once);
                        let persisted = scope == GrantScope::Permanent;
                        let grant = capability
                            .to_grant(scope, persisted)
                            .with_task_id(request.task_id.clone())
                            .with_session_id(request.session_id.clone());
                        manager.add_grant(grant);
                        manager.record_approval_result(
                            &request,
                            true,
                            format!("approved with {} scope", scope.as_str()),
                        );
                        manager
                            .authorize(&request, &self.workspace_root)
                            .map_err(|err| Error::Capability(Box::new(err)))
                    }
                    ApprovalCheck::Pending(entry) => {
                        Err(Error::ApprovalRequired(Box::new(entry.request)))
                    }
                    ApprovalCheck::Denied(entry) => {
                        manager.record_approval_result(&request, false, "approval denied");
                        Err(Error::Capability(Box::new(CapabilityError::Denied {
                            kind: request.kind,
                            target: request.target,
                            mode: request.mode,
                            risk: request.risk,
                            reason: format!("approval denied for {}", entry.request.short_summary),
                        })))
                    }
                    ApprovalCheck::Expired(entry) | ApprovalCheck::Superseded(entry) => {
                        manager.record_approval_result(
                            &request,
                            false,
                            format!("approval {}", entry.state.label()),
                        );
                        Err(Error::Capability(Box::new(
                            CapabilityError::NeedsApproval {
                                request_id: entry.request.id,
                                kind: request.kind,
                                target: request.target,
                                mode: request.mode,
                                risk: request.risk,
                                reason: format!("approval {}", entry.state.label()),
                                approval_options: Vec::new(),
                            },
                        )))
                    }
                }
            }
        }
    }

    fn resolve_access_target(&self, path: &str) -> Result<PathBuf, super::Error> {
        let raw = expand_home(path);
        let candidate = if raw.is_absolute() {
            raw
        } else {
            self.workspace_root.join(raw)
        };
        let normalized = normalize_path(candidate);
        if let Ok(canonical) = normalized.canonicalize() {
            Ok(canonical)
        } else {
            Ok(normalized)
        }
    }
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn path_in_workspace(target: &Path, workspace_root: &Path) -> bool {
    let workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(workspace_root.to_path_buf()));
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(target.to_path_buf()));
    target.starts_with(workspace)
}

#[derive(Debug, Clone)]
pub struct ToolContext<'a> {
    pub(crate) exec: &'a ExecutionContext,
    pub runtime: &'a RuntimeOptions,
}

impl<'a> ToolContext<'a> {
    pub fn new(exec: &'a ExecutionContext, runtime: &'a RuntimeOptions) -> Self {
        Self { exec, runtime }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.exec.workspace_root
    }

    pub fn is_cancelled(&self) -> bool {
        self.exec.is_cancelled()
    }

    pub fn cancel_token(&self) -> Option<&CancellationToken> {
        self.exec.cancel_token()
    }

    pub fn secrets(&self) -> &SecretRegistry {
        self.exec.secrets()
    }

    pub fn run_trust_context(&self) -> &RunTrustContext {
        self.exec.run_trust_context()
    }

    pub fn run_snapshot_store(&self) -> Option<&WorkspaceRunSnapshotStore> {
        self.exec.run_snapshot_store()
    }

    pub fn capability_manager(&self) -> Option<&CapabilityManager> {
        self.exec.capability_manager()
    }

    pub fn resolve_path(&self, relative_path: &str) -> Result<PathBuf, super::Error> {
        self.exec.resolve_path(relative_path)
    }

    pub fn resolve_path_for_access(
        &self,
        path: &str,
        mode: AccessMode,
        reason: impl Into<String>,
    ) -> Result<PathBuf, super::Error> {
        self.exec.resolve_path_for_access(path, mode, reason)
    }

    pub fn authorize_capability(&self, request: CapabilityRequest) -> Result<(), super::Error> {
        self.exec.authorize_capability(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{InfluenceMode, SourceKind, SourceLabel};
    use crate::run_snapshot::WorkspaceRunSnapshotStore;
    use std::fs;
    use tempfile::TempDir;

    fn create_context() -> (ExecutionContext, TempDir) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        (ExecutionContext::new(root), temp)
    }

    #[test]
    fn test_resolve_simple_relative_path() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("src/main.rs").unwrap();
        assert!(path.to_string_lossy().ends_with("src/main.rs"));
    }

    #[test]
    fn test_resolve_nested_path() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("a/b/c.txt").unwrap();
        assert!(path.to_string_lossy().ends_with("a/b/c.txt"));
    }

    #[test]
    fn test_reject_absolute_path() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("/etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_parent_traversal() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_nested_parent_traversal() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("a/../../b");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_double_dot_in_path() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("a/b/../c");
        assert!(result.is_err());
    }

    #[test]
    fn test_write_inside_workspace() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("test.txt").unwrap();
        fs::write(&path, "hello").unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(path).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_memory_context_round_trip() {
        let (ctx, _temp) = create_context();
        let ctx = ctx
            .with_memory_context("memory")
            .with_operator_context("operator");
        assert_eq!(ctx.memory_context(), Some("memory"));
        assert_eq!(ctx.operator_context(), Some("operator"));
    }

    #[test]
    fn test_run_snapshot_store_round_trip() {
        let (ctx, temp) = create_context();
        let run_snapshot = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        let ctx = ctx.with_workspace_run_snapshot_store(run_snapshot);
        assert_eq!(
            ctx.run_snapshot_store().unwrap().latest_status().unwrap(),
            None
        );
    }

    #[test]
    fn test_run_trust_context_round_trip() {
        let (ctx, _temp) = create_context();
        let mut trust = RunTrustContext::default();
        trust.add_source(SourceLabel::trusted(
            SourceKind::OperatorDirect,
            InfluenceMode::MayDriveAction,
            "current operator instruction",
        ));
        let ctx = ctx.with_run_trust_context(trust.clone());
        assert_eq!(ctx.run_trust_context(), &trust);
    }

    #[test]
    fn test_tool_context_exposes_authorized_runtime_boundary() {
        use crate::capability::{AccessConfig, CapabilityProfile};

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let manager = CapabilityManager::new(
            AccessConfig::for_profile(CapabilityProfile::Workspace),
            Vec::new(),
            "test",
            "unit",
        );
        let exec =
            ExecutionContext::new(workspace.path().to_path_buf()).with_capability_manager(manager);
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        let workspace_target = ctx
            .resolve_path_for_access(
                "file.txt",
                AccessMode::Read,
                "read a workspace file through the skill context",
            )
            .unwrap();
        assert!(workspace_target.starts_with(ctx.workspace_root()));

        let outside_target = outside.path().join("file.txt");
        let denied = ctx.resolve_path_for_access(
            outside_target.to_str().unwrap(),
            AccessMode::Read,
            "read a file outside the workspace through the skill context",
        );
        assert!(denied.is_err());
        assert!(denied.unwrap_err().to_string().contains("approval"));
    }
}

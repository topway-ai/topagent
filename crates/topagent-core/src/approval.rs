use crate::cancel::CancellationToken;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub mailbox_available: bool,
    pub triggers: &'static [ApprovalTriggerRule],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalTriggerRule {
    pub kind: ApprovalTriggerKind,
    pub label: &'static str,
    pub enforcement: ApprovalEnforcement,
    pub rationale: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTriggerKind {
    GitCommit,
    DestructiveShellMutation,
    HostExternalExecution,
    GeneratedToolDeletion,
}

impl ApprovalTriggerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GitCommit => "git commit",
            Self::DestructiveShellMutation => "shell mutation",
            Self::HostExternalExecution => "host external tool execution",
            Self::GeneratedToolDeletion => "generated tool deletion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalEnforcement {
    AdvisoryOnly,
    RequiredWhenAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestDraft {
    pub action_kind: ApprovalTriggerKind,
    pub short_summary: String,
    pub exact_action: String,
    pub reason: String,
    pub scope_of_impact: String,
    pub expected_effect: String,
    pub rollback_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: String,
    pub action_kind: ApprovalTriggerKind,
    pub short_summary: String,
    pub exact_action: String,
    pub reason: String,
    pub scope_of_impact: String,
    pub expected_effect: String,
    pub rollback_hint: Option<String>,
    pub created_at: SystemTime,
}

impl ApprovalRequest {
    pub fn render_details(&self) -> String {
        let mut body = format!(
            "Approval required\n\n\
             Id: {}\n\
             Action: {}\n\
             Summary: {}\n\
             Requested: {}\n\
             Reason: {}\n\
             Scope: {}\n\
             Expected effect: {}",
            self.id,
            self.action_kind.label(),
            self.short_summary,
            self.exact_action,
            self.reason,
            self.scope_of_impact,
            self.expected_effect,
        );

        if let Some(rollback_hint) = &self.rollback_hint {
            body.push_str(&format!("\nRollback hint: {rollback_hint}"));
        }

        body
    }

    pub fn render_status_line(&self, state: ApprovalState) -> String {
        format!("{} [{}] {}", self.id, state.label(), self.short_summary)
    }
}

impl fmt::Display for ApprovalRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.id, self.short_summary)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Superseded,
}

impl ApprovalState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalEntry {
    pub request: ApprovalRequest,
    pub state: ApprovalState,
    pub resolved_at: Option<SystemTime>,
    pub decision_note: Option<String>,
}

impl ApprovalEntry {
    pub fn is_pending(&self) -> bool {
        self.state == ApprovalState::Pending
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheck {
    Approved(ApprovalEntry),
    Pending(ApprovalEntry),
    Denied(ApprovalEntry),
    Expired(ApprovalEntry),
    Superseded(ApprovalEntry),
}

impl ApprovalCheck {
    fn from_entry(entry: ApprovalEntry) -> Self {
        match entry.state {
            ApprovalState::Approved => Self::Approved(entry),
            ApprovalState::Pending => Self::Pending(entry),
            ApprovalState::Denied => Self::Denied(entry),
            ApprovalState::Expired => Self::Expired(entry),
            ApprovalState::Superseded => Self::Superseded(entry),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMailboxMode {
    Wait,
    Immediate,
}

pub type ApprovalNotifier = Arc<dyn Fn(ApprovalRequest) + Send + Sync + 'static>;

struct ApprovalMailboxInner {
    mode: ApprovalMailboxMode,
    state: Mutex<ApprovalMailboxState>,
    notifier: Mutex<Option<ApprovalNotifier>>,
    condvar: Condvar,
}

#[derive(Debug, Default)]
struct ApprovalMailboxState {
    next_id: u64,
    entries: Vec<ApprovalEntry>,
}

#[derive(Clone)]
pub struct ApprovalMailbox {
    inner: Arc<ApprovalMailboxInner>,
}

impl fmt::Debug for ApprovalMailbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.lock().unwrap();
        f.debug_struct("ApprovalMailbox")
            .field("mode", &self.inner.mode)
            .field("entries", &state.entries.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResolveError {
    NotFound(String),
    NotPending { id: String, state: ApprovalState },
    InvalidState(ApprovalState),
}

impl fmt::Display for ApprovalResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "approval request not found: {id}"),
            Self::NotPending { id, state } => {
                write!(f, "approval request {id} is already {}", state.label())
            }
            Self::InvalidState(state) => {
                write!(f, "cannot resolve approval request to {}", state.label())
            }
        }
    }
}

impl ApprovalMailbox {
    pub fn new(mode: ApprovalMailboxMode) -> Self {
        Self {
            inner: Arc::new(ApprovalMailboxInner {
                mode,
                state: Mutex::new(ApprovalMailboxState::default()),
                notifier: Mutex::new(None),
                condvar: Condvar::new(),
            }),
        }
    }

    pub fn mode(&self) -> ApprovalMailboxMode {
        self.inner.mode
    }

    pub fn set_notifier(&self, notifier: ApprovalNotifier) {
        *self.inner.notifier.lock().unwrap() = Some(notifier);
    }

    pub fn request_decision(
        &self,
        draft: ApprovalRequestDraft,
        cancel: Option<&CancellationToken>,
    ) -> ApprovalCheck {
        let (entry, is_new) = {
            let mut state = self.inner.state.lock().unwrap();
            if let Some(existing) = state
                .entries
                .iter()
                .rev()
                .find(|entry| {
                    entry.request.action_kind == draft.action_kind
                        && entry.request.exact_action == draft.exact_action
                })
                .cloned()
            {
                (existing, false)
            } else {
                state.next_id += 1;
                let request = ApprovalRequest {
                    id: format!("apr-{}", state.next_id),
                    action_kind: draft.action_kind,
                    short_summary: draft.short_summary,
                    exact_action: draft.exact_action,
                    reason: draft.reason,
                    scope_of_impact: draft.scope_of_impact,
                    expected_effect: draft.expected_effect,
                    rollback_hint: draft.rollback_hint,
                    created_at: SystemTime::now(),
                };
                let entry = ApprovalEntry {
                    request,
                    state: ApprovalState::Pending,
                    resolved_at: None,
                    decision_note: None,
                };
                state.entries.push(entry.clone());
                (entry, true)
            }
        };

        if is_new {
            if let Some(notifier) = self.inner.notifier.lock().unwrap().clone() {
                notifier(entry.request.clone());
            }
        }

        match self.mode() {
            ApprovalMailboxMode::Wait => self.wait_for_resolution(&entry.request.id, cancel),
            ApprovalMailboxMode::Immediate => self
                .get(&entry.request.id)
                .map(ApprovalCheck::from_entry)
                .unwrap_or(ApprovalCheck::Pending(entry)),
        }
    }

    pub fn list(&self) -> Vec<ApprovalEntry> {
        self.inner.state.lock().unwrap().entries.clone()
    }

    pub fn pending(&self) -> Vec<ApprovalEntry> {
        self.list()
            .into_iter()
            .filter(ApprovalEntry::is_pending)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<ApprovalEntry> {
        self.inner
            .state
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.request.id == id)
            .cloned()
    }

    pub fn approve(
        &self,
        id: &str,
        note: Option<String>,
    ) -> std::result::Result<ApprovalEntry, ApprovalResolveError> {
        self.resolve(id, ApprovalState::Approved, note)
    }

    pub fn deny(
        &self,
        id: &str,
        note: Option<String>,
    ) -> std::result::Result<ApprovalEntry, ApprovalResolveError> {
        self.resolve(id, ApprovalState::Denied, note)
    }

    pub fn expire_pending(&self, note: impl Into<String>) -> usize {
        self.resolve_all_pending(ApprovalState::Expired, note.into())
    }

    pub fn supersede_pending(&self, note: impl Into<String>) -> usize {
        self.resolve_all_pending(ApprovalState::Superseded, note.into())
    }

    fn wait_for_resolution(&self, id: &str, cancel: Option<&CancellationToken>) -> ApprovalCheck {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if let Some(entry) = state
                .entries
                .iter()
                .find(|entry| entry.request.id == id)
                .cloned()
            {
                if entry.state != ApprovalState::Pending {
                    return ApprovalCheck::from_entry(entry);
                }
            } else {
                let synthetic = ApprovalEntry {
                    request: ApprovalRequest {
                        id: id.to_string(),
                        action_kind: ApprovalTriggerKind::DestructiveShellMutation,
                        short_summary: "approval request missing from mailbox".to_string(),
                        exact_action: id.to_string(),
                        reason: "mailbox state changed unexpectedly".to_string(),
                        scope_of_impact: "unknown".to_string(),
                        expected_effect: "approval could not be resolved".to_string(),
                        rollback_hint: None,
                        created_at: SystemTime::now(),
                    },
                    state: ApprovalState::Expired,
                    resolved_at: Some(SystemTime::now()),
                    decision_note: Some("approval request disappeared from mailbox".to_string()),
                };
                return ApprovalCheck::Expired(synthetic);
            }

            if cancel.is_some_and(|token| token.is_cancelled()) {
                let entry = Self::resolve_locked(
                    &mut state,
                    id,
                    ApprovalState::Expired,
                    Some("task stopped before approval was resolved".to_string()),
                )
                .ok()
                .or_else(|| {
                    state
                        .entries
                        .iter()
                        .find(|entry| entry.request.id == id)
                        .cloned()
                })
                .expect("pending approval request should still exist");
                self.inner.condvar.notify_all();
                return ApprovalCheck::Expired(entry);
            }

            let (next_state, _) = self
                .inner
                .condvar
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap();
            state = next_state;
        }
    }

    fn resolve(
        &self,
        id: &str,
        target: ApprovalState,
        note: Option<String>,
    ) -> std::result::Result<ApprovalEntry, ApprovalResolveError> {
        let mut state = self.inner.state.lock().unwrap();
        let entry = Self::resolve_locked(&mut state, id, target, note)?;
        self.inner.condvar.notify_all();
        Ok(entry)
    }

    fn resolve_locked(
        state: &mut ApprovalMailboxState,
        id: &str,
        target: ApprovalState,
        note: Option<String>,
    ) -> std::result::Result<ApprovalEntry, ApprovalResolveError> {
        if target == ApprovalState::Pending {
            return Err(ApprovalResolveError::InvalidState(target));
        }

        let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.request.id == id)
        else {
            return Err(ApprovalResolveError::NotFound(id.to_string()));
        };

        if entry.state != ApprovalState::Pending {
            return Err(ApprovalResolveError::NotPending {
                id: id.to_string(),
                state: entry.state,
            });
        }

        entry.state = target;
        entry.resolved_at = Some(SystemTime::now());
        entry.decision_note = note;
        Ok(entry.clone())
    }

    fn resolve_all_pending(&self, target: ApprovalState, note: String) -> usize {
        let mut state = self.inner.state.lock().unwrap();
        let mut resolved = 0usize;
        for entry in &mut state.entries {
            if entry.state != ApprovalState::Pending {
                continue;
            }
            entry.state = target;
            entry.resolved_at = Some(SystemTime::now());
            entry.decision_note = Some(note.clone());
            resolved += 1;
        }
        if resolved > 0 {
            self.inner.condvar.notify_all();
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> ApprovalRequestDraft {
        ApprovalRequestDraft {
            action_kind: ApprovalTriggerKind::GitCommit,
            short_summary: "git commit: release notes".to_string(),
            exact_action: "git_commit(message=\"release notes\")".to_string(),
            reason: "commits publish a durable repo milestone".to_string(),
            scope_of_impact: "Creates a new git commit in the workspace repository.".to_string(),
            expected_effect: "Staged changes become a durable commit.".to_string(),
            rollback_hint: Some("Use git revert or git reset if the commit was mistaken.".into()),
        }
    }

    #[test]
    fn test_mailbox_creates_inspectable_request() {
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);

        let check = mailbox.request_decision(sample_request(), None);
        let entry = match check {
            ApprovalCheck::Pending(entry) => entry,
            other => panic!("expected pending entry, got {other:?}"),
        };

        assert_eq!(entry.request.id, "apr-1");
        assert_eq!(entry.request.action_kind, ApprovalTriggerKind::GitCommit);
        assert!(entry.request.render_details().contains("Approval required"));
        assert_eq!(mailbox.pending().len(), 1);
    }

    #[test]
    fn test_mailbox_state_transitions_are_inspectable() {
        let mailbox = ApprovalMailbox::new(ApprovalMailboxMode::Immediate);
        let first = mailbox.request_decision(sample_request(), None);
        let first_id = match first {
            ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending entry, got {other:?}"),
        };

        let approved = mailbox
            .approve(&first_id, Some("operator approved".into()))
            .unwrap();
        assert_eq!(approved.state, ApprovalState::Approved);
        assert_eq!(
            mailbox
                .get(&first_id)
                .expect("request should still be inspectable")
                .state,
            ApprovalState::Approved
        );

        let denied = mailbox.request_decision(
            ApprovalRequestDraft {
                exact_action: "git_commit(message=\"release notes v2\")".into(),
                short_summary: "git commit: release notes v2".into(),
                ..sample_request()
            },
            None,
        );
        let denied_id = match denied {
            ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending entry, got {other:?}"),
        };
        let denied = mailbox
            .deny(&denied_id, Some("operator denied".into()))
            .unwrap();
        assert_eq!(denied.state, ApprovalState::Denied);

        let expired = mailbox.request_decision(
            ApprovalRequestDraft {
                exact_action: "git_commit(message=\"release notes v3\")".into(),
                short_summary: "git commit: release notes v3".into(),
                ..sample_request()
            },
            None,
        );
        let expired_id = match expired {
            ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending entry, got {other:?}"),
        };
        assert_eq!(
            mailbox.expire_pending("task stopped before approval was resolved"),
            1
        );
        assert_eq!(
            mailbox.get(&expired_id).unwrap().state,
            ApprovalState::Expired
        );

        let superseded = mailbox.request_decision(
            ApprovalRequestDraft {
                exact_action: "git_commit(message=\"release notes v4\")".into(),
                short_summary: "git commit: release notes v4".into(),
                ..sample_request()
            },
            None,
        );
        let superseded_id = match superseded {
            ApprovalCheck::Pending(entry) => entry.request.id,
            other => panic!("expected pending entry, got {other:?}"),
        };
        assert_eq!(mailbox.supersede_pending("chat reset"), 1);
        assert_eq!(
            mailbox.get(&superseded_id).unwrap().state,
            ApprovalState::Superseded
        );
    }
}

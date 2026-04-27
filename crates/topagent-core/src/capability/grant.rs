use crate::capability::{CapabilityKind, CapabilityRequest};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Read,
    Write,
    Execute,
    ReadWrite,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::ReadWrite => "read_write",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::ReadWrite => "read/write",
        }
    }

    pub fn allows(self, requested: AccessMode) -> bool {
        self == requested
            || matches!(
                (self, requested),
                (AccessMode::ReadWrite, AccessMode::Read)
                    | (AccessMode::ReadWrite, AccessMode::Write)
            )
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AccessMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "execute" | "exec" | "run" => Ok(Self::Execute),
            "read_write" | "readwrite" | "rw" => Ok(Self::ReadWrite),
            other => Err(format!(
                "unknown access mode `{other}` (expected read, write, execute, or read_write)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    Once,
    ThisTask,
    ThisPath,
    Session,
    Permanent,
}

impl GrantScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::ThisTask => "task",
            Self::ThisPath => "path",
            Self::Session => "session",
            Self::Permanent => "permanent",
        }
    }

    pub fn is_temporary(self) -> bool {
        !matches!(self, Self::Permanent)
    }
}

impl fmt::Display for GrantScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GrantScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "once" => Ok(Self::Once),
            "task" | "this_task" => Ok(Self::ThisTask),
            "path" | "this_path" => Ok(Self::ThisPath),
            "session" => Ok(Self::Session),
            "permanent" | "persist" => Ok(Self::Permanent),
            other => Err(format!(
                "unknown grant scope `{other}` (expected once, task, path, session, or permanent)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub id: String,
    pub kind: CapabilityKind,
    pub target: String,
    pub mode: AccessMode,
    pub scope: GrantScope,
    pub created_at_unix: u64,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub uses_remaining: Option<u32>,
    pub persisted: bool,
    pub reason: String,
}

impl CapabilityGrant {
    pub fn new(
        kind: CapabilityKind,
        target: impl Into<String>,
        mode: AccessMode,
        scope: GrantScope,
        reason: impl Into<String>,
    ) -> Self {
        let now = unix_now();
        Self {
            id: format!("grant-{now}"),
            kind,
            target: target.into(),
            mode,
            scope,
            created_at_unix: now,
            task_id: None,
            session_id: None,
            uses_remaining: matches!(scope, GrantScope::Once).then_some(1),
            persisted: false,
            reason: reason.into(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_task_id(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }

    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn persisted(mut self, persisted: bool) -> Self {
        self.persisted = persisted;
        self
    }

    pub fn is_expired(&self) -> bool {
        self.uses_remaining == Some(0)
    }

    pub fn is_temporary(&self) -> bool {
        self.scope.is_temporary()
    }

    pub(crate) fn matches_request(&self, request: &CapabilityRequest) -> bool {
        self.kind == request.kind
            && self.mode.allows(request.mode)
            && self.scope_matches(request)
            && self.target_matches(request)
            && !self.is_expired()
    }

    pub(crate) fn bind_scope_if_needed(&mut self, request: &CapabilityRequest) {
        if self.scope == GrantScope::ThisTask && self.task_id.is_none() {
            self.task_id = request.task_id.clone();
        }
        if self.scope == GrantScope::Session && self.session_id.is_none() {
            self.session_id = request.session_id.clone();
        }
    }

    pub(crate) fn consume(&mut self) {
        if let Some(uses) = self.uses_remaining.as_mut() {
            *uses = uses.saturating_sub(1);
        }
    }

    fn scope_matches(&self, request: &CapabilityRequest) -> bool {
        match self.scope {
            GrantScope::Once | GrantScope::ThisPath | GrantScope::Permanent => true,
            GrantScope::ThisTask => self
                .task_id
                .as_ref()
                .map_or(request.task_id.is_some(), |task_id| {
                    request.task_id.as_ref() == Some(task_id)
                }),
            GrantScope::Session => self
                .session_id
                .as_ref()
                .map_or(request.session_id.is_some(), |session_id| {
                    request.session_id.as_ref() == Some(session_id)
                }),
        }
    }

    fn target_matches(&self, request: &CapabilityRequest) -> bool {
        if self.target == "*" || self.target == request.target {
            return true;
        }

        match self.kind {
            CapabilityKind::Filesystem => path_prefix_matches(&self.target, &request.target),
            CapabilityKind::Network | CapabilityKind::WebSearch => {
                request.target.starts_with(&self.target)
            }
            _ => false,
        }
    }
}

fn path_prefix_matches(grant_target: &str, request_target: &str) -> bool {
    if request_target == grant_target {
        return true;
    }
    let grant = grant_target.trim_end_matches('/');
    request_target
        .strip_prefix(grant)
        .is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

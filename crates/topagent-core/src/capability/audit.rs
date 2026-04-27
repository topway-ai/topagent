use crate::capability::{
    redact_sensitive_target, AccessMode, CapabilityKind, CapabilityProfile, RiskLevel,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    ProfileChanged,
    GrantCreated,
    GrantUsed,
    GrantExpired,
    GrantRevoked,
    ApprovalRequested,
    ApprovalAccepted,
    ApprovalDenied,
    HighRiskAllowed,
    HighRiskBlocked,
    LockdownActivated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAuditRecord {
    pub timestamp_unix: u64,
    pub event: AuditEvent,
    pub actor: String,
    pub source: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: Option<CapabilityKind>,
    pub target: Option<String>,
    pub mode: Option<AccessMode>,
    pub risk: Option<RiskLevel>,
    pub profile: CapabilityProfile,
    pub decision: String,
    pub reason: String,
}

impl CapabilityAuditRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event: AuditEvent,
        actor: impl Into<String>,
        source: impl Into<String>,
        task_id: Option<String>,
        session_id: Option<String>,
        kind: Option<CapabilityKind>,
        target: Option<String>,
        mode: Option<AccessMode>,
        risk: Option<RiskLevel>,
        profile: CapabilityProfile,
        decision: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            timestamp_unix: crate::capability::grant::unix_now(),
            event,
            actor: actor.into(),
            source: source.into(),
            task_id,
            session_id,
            kind,
            target: target.map(|value| redact_sensitive_target(&value)),
            mode,
            risk,
            profile,
            decision: decision.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAuditLog {
    path: PathBuf,
}

impl CapabilityAuditLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &CapabilityAuditRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(record)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        line.push('\n');
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    pub fn read_recent(&self, limit: usize) -> std::io::Result<Vec<CapabilityAuditRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let contents = std::fs::read_to_string(&self.path)?;
        let mut records = contents
            .lines()
            .rev()
            .take(limit)
            .filter_map(|line| serde_json::from_str::<CapabilityAuditRecord>(line).ok())
            .collect::<Vec<_>>();
        records.reverse();
        Ok(records)
    }
}

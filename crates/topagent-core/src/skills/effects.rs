use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillEffect {
    ReadFilesystem,
    WriteFilesystem,
    ExecuteCommand,
    NetworkAccess,
    WebSearch,
    GitRead,
    GitWrite,
    PackageInstall,
    MemoryRead,
    MemoryWrite,
    ComputerUse,
    ExternalSend,
    SystemChange,
    SecretAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillEffects {
    pub effects: Vec<SkillEffect>,
    pub read_only: bool,
    pub mutating: bool,
    pub destructive: bool,
    pub parallel_safe: bool,
    pub workspace_scoped: bool,
    pub outside_workspace_capable: bool,
}

impl SkillEffects {
    pub fn new(effects: impl Into<Vec<SkillEffect>>) -> Self {
        Self {
            effects: effects.into(),
            read_only: false,
            mutating: false,
            destructive: false,
            parallel_safe: false,
            workspace_scoped: true,
            outside_workspace_capable: false,
        }
    }

    pub fn read_only(effects: impl Into<Vec<SkillEffect>>) -> Self {
        Self::new(effects)
            .with_read_only(true)
            .with_parallel_safe(true)
    }

    pub fn mutating(effects: impl Into<Vec<SkillEffect>>) -> Self {
        Self::new(effects)
            .with_read_only(false)
            .with_mutating(true)
            .with_parallel_safe(false)
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        if read_only {
            self.mutating = false;
            self.destructive = false;
        }
        self
    }

    pub fn with_mutating(mut self, mutating: bool) -> Self {
        self.mutating = mutating;
        if mutating {
            self.read_only = false;
        }
        self
    }

    pub fn with_destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        if destructive {
            self.mutating = true;
            self.read_only = false;
            self.parallel_safe = false;
        }
        self
    }

    pub fn with_parallel_safe(mut self, parallel_safe: bool) -> Self {
        self.parallel_safe = parallel_safe;
        self
    }

    pub fn with_workspace_scoped(mut self, workspace_scoped: bool) -> Self {
        self.workspace_scoped = workspace_scoped;
        self
    }

    pub fn with_outside_workspace_capable(mut self, outside_workspace_capable: bool) -> Self {
        self.outside_workspace_capable = outside_workspace_capable;
        self
    }

    pub fn includes(&self, effect: SkillEffect) -> bool {
        self.effects.contains(&effect)
    }
}

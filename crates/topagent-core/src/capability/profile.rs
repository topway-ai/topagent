use crate::capability::CapabilityGrant;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityProfile {
    Workspace,
    Developer,
    Computer,
    Full,
}

impl Default for CapabilityProfile {
    fn default() -> Self {
        Self::Developer
    }
}

impl CapabilityProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Developer => "developer",
            Self::Computer => "computer",
            Self::Full => "full",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Developer => "developer",
            Self::Computer => "computer",
            Self::Full => "full",
        }
    }
}

impl fmt::Display for CapabilityProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CapabilityProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "workspace" => Ok(Self::Workspace),
            "developer" => Ok(Self::Developer),
            "computer" => Ok(Self::Computer),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown access profile `{other}` (expected workspace, developer, computer, or full)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Filesystem,
    Shell,
    Network,
    WebSearch,
    Git,
    PackageManager,
    ComputerUse,
    MemoryWrite,
    ExternalSend,
    SystemService,
    SecretRead,
}

impl CapabilityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Shell => "shell",
            Self::Network => "network",
            Self::WebSearch => "web_search",
            Self::Git => "git",
            Self::PackageManager => "package_manager",
            Self::ComputerUse => "computer_use",
            Self::MemoryWrite => "memory_write",
            Self::ExternalSend => "external_send",
            Self::SystemService => "system_service",
            Self::SecretRead => "secret_read",
        }
    }
}

impl fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CapabilityKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "filesystem" | "file" | "path" => Ok(Self::Filesystem),
            "shell" | "bash" => Ok(Self::Shell),
            "network" | "net" => Ok(Self::Network),
            "web_search" | "web-search" | "web" | "search" => Ok(Self::WebSearch),
            "git" => Ok(Self::Git),
            "package_manager" | "package-manager" | "packages" | "package" => {
                Ok(Self::PackageManager)
            }
            "computer_use" | "computer-use" | "computer" => Ok(Self::ComputerUse),
            "memory_write" | "memory-write" | "memory" => Ok(Self::MemoryWrite),
            "external_send" | "external-send" | "send" => Ok(Self::ExternalSend),
            "system_service" | "system-service" | "service" => Ok(Self::SystemService),
            "secret_read" | "secret-read" | "secret" => Ok(Self::SecretRead),
            other => Err(format!("unknown capability kind `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessConfig {
    pub profile: CapabilityProfile,
    pub network_default: bool,
    pub web_search_default: bool,
    pub computer_use_default: bool,
    pub allow_workspace_write: bool,
    pub allow_home_read: bool,
    pub allow_home_write: bool,
    pub require_approval_for_sudo: bool,
    pub require_approval_for_destructive: bool,
    pub require_approval_for_secret_paths: bool,
    pub require_approval_for_external_send: bool,
    pub require_approval_for_global_package_install: bool,
    pub require_approval_for_git_push: bool,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self::for_profile(CapabilityProfile::Developer)
    }
}

impl AccessConfig {
    pub fn for_profile(profile: CapabilityProfile) -> Self {
        let developer_like = matches!(
            profile,
            CapabilityProfile::Developer | CapabilityProfile::Computer | CapabilityProfile::Full
        );
        Self {
            profile,
            network_default: developer_like,
            web_search_default: developer_like,
            computer_use_default: matches!(profile, CapabilityProfile::Computer),
            allow_workspace_write: true,
            allow_home_read: false,
            allow_home_write: false,
            require_approval_for_sudo: true,
            require_approval_for_destructive: true,
            require_approval_for_secret_paths: true,
            require_approval_for_external_send: true,
            require_approval_for_global_package_install: true,
            require_approval_for_git_push: true,
        }
    }

    pub fn set_profile_defaults(&mut self, profile: CapabilityProfile) {
        let current = Self::for_profile(profile);
        self.profile = current.profile;
        self.network_default = current.network_default;
        self.web_search_default = current.web_search_default;
        self.computer_use_default = current.computer_use_default;
        self.allow_workspace_write = current.allow_workspace_write;
    }

    pub fn lockdown(&mut self) {
        self.profile = CapabilityProfile::Workspace;
        self.network_default = false;
        self.web_search_default = false;
        self.computer_use_default = false;
        self.allow_home_read = false;
        self.allow_home_write = false;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccessConfigDocument {
    pub access: AccessConfig,
    pub grants: Vec<CapabilityGrant>,
}

impl AccessConfigDocument {
    pub fn load_or_default(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)?;
        toml::from_str(&contents)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
    }

    pub fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

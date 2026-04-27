use crate::capability::CapabilityKind;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Moderate,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Moderate => "Moderate",
            Self::High => "High",
            Self::Critical => "Critical",
        }
    }

    pub fn is_high_impact(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAssessment {
    pub kind: CapabilityKind,
    pub risk: RiskLevel,
    pub reason: String,
    pub network_required: bool,
    pub high_impact: bool,
}

pub fn assess_shell_command(command: &str) -> ShellAssessment {
    let lower = command.trim().to_ascii_lowercase();
    let network_required = command_uses_network(&lower);

    if contains_shell_pipe_to_interpreter(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::PackageManager,
            risk: RiskLevel::Critical,
            reason: "downloads remote content and executes it through a shell".to_string(),
            network_required: true,
            high_impact: true,
        };
    }

    if contains_standalone_command(&lower, "sudo") {
        return ShellAssessment {
            kind: CapabilityKind::Shell,
            risk: RiskLevel::Critical,
            reason: "sudo can change system-level state".to_string(),
            network_required,
            high_impact: true,
        };
    }

    if lower.contains("rm -rf /")
        || lower.contains("rm -fr /")
        || lower.contains("rm -rf *")
        || lower.contains("rm -fr *")
    {
        return ShellAssessment {
            kind: CapabilityKind::Filesystem,
            risk: RiskLevel::Critical,
            reason: "recursive destructive delete pattern".to_string(),
            network_required,
            high_impact: true,
        };
    }

    if starts_with_any_command(&lower, &["mkfs", "fdisk", "parted", "sfdisk", "wipefs"])
        || contains_standalone_command(&lower, "dd")
    {
        return ShellAssessment {
            kind: CapabilityKind::SystemService,
            risk: RiskLevel::Critical,
            reason: "disk or filesystem operation can destroy data".to_string(),
            network_required,
            high_impact: true,
        };
    }

    if service_change_command(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::SystemService,
            risk: RiskLevel::High,
            reason: "system service changes affect host lifecycle".to_string(),
            network_required,
            high_impact: true,
        };
    }

    if lower.contains("git push") {
        return ShellAssessment {
            kind: CapabilityKind::Git,
            risk: RiskLevel::High,
            reason: "git push publishes local state to a remote repository".to_string(),
            network_required: true,
            high_impact: true,
        };
    }

    if external_send_command(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::ExternalSend,
            risk: RiskLevel::High,
            reason: "command sends, posts, uploads, deploys, or releases external state"
                .to_string(),
            network_required: true,
            high_impact: true,
        };
    }

    if global_package_install_command(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::PackageManager,
            risk: RiskLevel::High,
            reason: "global package install changes system or user-wide tool state".to_string(),
            network_required: true,
            high_impact: true,
        };
    }

    if destructive_shell_command(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::Filesystem,
            risk: RiskLevel::High,
            reason: "destructive shell command can remove or overwrite files".to_string(),
            network_required,
            high_impact: true,
        };
    }

    if package_manager_command(&lower) {
        return ShellAssessment {
            kind: CapabilityKind::PackageManager,
            risk: RiskLevel::Moderate,
            reason: "package manager or build workflow".to_string(),
            network_required,
            high_impact: false,
        };
    }

    if lower.starts_with("git ") {
        return ShellAssessment {
            kind: CapabilityKind::Git,
            risk: RiskLevel::Safe,
            reason: "git inspection or local workflow command".to_string(),
            network_required,
            high_impact: false,
        };
    }

    if network_required {
        return ShellAssessment {
            kind: CapabilityKind::Network,
            risk: RiskLevel::Safe,
            reason: "network read command".to_string(),
            network_required: true,
            high_impact: false,
        };
    }

    ShellAssessment {
        kind: CapabilityKind::Shell,
        risk: RiskLevel::Safe,
        reason: "workspace shell command".to_string(),
        network_required: false,
        high_impact: false,
    }
}

pub fn assess_computer_action(action: &str, target: &str) -> (RiskLevel, String) {
    let text = format!("{} {}", action, target).to_ascii_lowercase();
    let high_terms = [
        "send",
        "email",
        "message",
        "upload",
        "purchase",
        "payment",
        "checkout",
        "submit",
        "delete",
        "remove",
        "security",
        "password",
        "2fa",
        "legal",
        "government",
        "financial",
        "bank",
        "deploy",
        "release",
    ];
    if high_terms.iter().any(|term| text.contains(term)) {
        (
            RiskLevel::High,
            "high-impact UI action requires explicit approval".to_string(),
        )
    } else {
        (
            RiskLevel::Moderate,
            "controlled computer_use harness action".to_string(),
        )
    }
}

pub fn is_secret_target(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    let normalized = lower.replace('\\', "/");
    normalized.contains("/.ssh/")
        || normalized.ends_with("/.ssh")
        || normalized.contains("/.gnupg/")
        || normalized.contains("/.aws/credentials")
        || normalized.contains("/.config/gcloud/")
        || normalized.contains("/.docker/config.json")
        || normalized.contains("topagent/services/topagent-telegram.env")
        || normalized.ends_with("topagent-telegram.env")
        || normalized.ends_with("/.env")
        || normalized.contains("/.env.")
        || normalized.ends_with("/id_rsa")
        || normalized.ends_with("/id_ed25519")
        || normalized.ends_with("/known_hosts")
}

pub fn redact_sensitive_target(target: &str) -> String {
    if !is_secret_target(target) {
        return target.to_string();
    }
    let Some((prefix, _)) = target.rsplit_once('/') else {
        return "[REDACTED_SECRET_PATH]".to_string();
    };
    format!("{prefix}/[REDACTED_SECRET_PATH]")
}

fn command_uses_network(lower: &str) -> bool {
    lower.contains("http://")
        || lower.contains("https://")
        || starts_with_any_command(lower, &["curl", "wget", "http", "https", "git clone"])
        || lower.starts_with("npm install")
        || lower.starts_with("pnpm install")
        || lower.starts_with("yarn add")
        || lower.starts_with("cargo fetch")
        || lower.starts_with("go get")
}

fn contains_shell_pipe_to_interpreter(lower: &str) -> bool {
    (lower.contains("curl ") || lower.contains("wget "))
        && (lower.contains("| sh")
            || lower.contains("| bash")
            || lower.contains("| zsh")
            || lower.contains("| fish"))
}

fn starts_with_any_command(lower: &str, commands: &[&str]) -> bool {
    commands.iter().any(|command| {
        lower == *command
            || lower
                .strip_prefix(command)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

fn contains_standalone_command(lower: &str, command: &str) -> bool {
    lower == command
        || lower.starts_with(&format!("{command} "))
        || lower.contains(&format!("; {command}"))
        || lower.contains(&format!("&& {command}"))
        || lower.contains(&format!("| {command}"))
}

fn service_change_command(lower: &str) -> bool {
    (starts_with_any_command(lower, &["systemctl", "service", "launchctl"])
        && !lower.contains(" status")
        && !lower.contains(" list")
        && !lower.contains(" show"))
        || lower.contains("systemctl --user enable")
        || lower.contains("systemctl --user disable")
}

fn external_send_command(lower: &str) -> bool {
    lower.contains("curl -x post")
        || lower.contains("curl -xput")
        || lower.contains("curl -x patch")
        || lower.contains("curl --upload-file")
        || lower.contains(" scp ")
        || lower.starts_with("scp ")
        || lower.contains(" rsync ")
        || lower.starts_with("rsync ")
        || lower.contains(" deploy")
        || lower.starts_with("deploy")
        || lower.contains(" release")
        || lower.starts_with("release")
        || lower.contains(" publish")
        || lower.starts_with("publish")
}

fn global_package_install_command(lower: &str) -> bool {
    lower.starts_with("apt install")
        || lower.starts_with("apt-get install")
        || lower.starts_with("dnf install")
        || lower.starts_with("yum install")
        || lower.starts_with("pacman -s")
        || lower.starts_with("apk add")
        || lower.starts_with("brew install")
        || lower.starts_with("npm install -g")
        || lower.starts_with("npm i -g")
        || lower.starts_with("pnpm add -g")
        || lower.starts_with("yarn global add")
        || lower.starts_with("pip install")
        || lower.starts_with("pip3 install")
        || lower.starts_with("gem install")
        || lower.starts_with("cargo install")
}

fn package_manager_command(lower: &str) -> bool {
    starts_with_any_command(
        lower,
        &[
            "cargo", "npm", "pnpm", "yarn", "pip", "pip3", "go", "make", "pytest",
        ],
    )
}

fn destructive_shell_command(lower: &str) -> bool {
    starts_with_any_command(
        lower,
        &["rm", "unlink", "rmdir", "mv", "chmod", "chown", "chgrp"],
    ) || lower.contains(" -delete")
        || lower.contains(" > ")
        || lower.contains(">>")
}

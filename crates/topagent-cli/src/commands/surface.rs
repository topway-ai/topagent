pub(crate) const PRODUCT_NAME: &str = "TopAgent";

pub(crate) const HELP_STATUS: &str = "Show TopAgent installation, service, and model status.";
pub(crate) const HELP_DOCTOR: &str =
    "Run health diagnostics on installation, config, workspace, and tools.";
pub(crate) const HELP_CONFIG_INSPECT: &str =
    "Show the resolved runtime contract (provider, model, keys, workspace, options).";
pub(crate) const HELP_RUN_STATUS: &str =
    "Show execution-session state: run snapshot, transcripts, and restore guidance.";
pub(crate) const HELP_RUN_DIFF: &str =
    "Preview the diff between the latest run snapshot and the current workspace.";
pub(crate) const HELP_RUN_RESTORE: &str =
    "Restore the latest run snapshot and clear persisted Telegram transcripts.";

pub(crate) const HELP_MODEL_STATUS: &str = "Show the configured default and effective model.";
pub(crate) const HELP_MODEL_SET: &str =
    "Set the configured model (does not change provider; to change provider, run topagent install).";
pub(crate) const HELP_MODEL_PICK: &str = "Pick the configured model interactively.";
pub(crate) const HELP_MODEL_LIST: &str = "Show the cached model list.";
pub(crate) const HELP_MODEL_REFRESH: &str = "Refresh the cached model list.";

pub(crate) const HELP_MEMORY_STATUS: &str = "Show workspace learning artifact status.";
pub(crate) const HELP_MEMORY_LINT: &str =
    "Lint USER.md and MEMORY.md for size, format, and content policy issues.";
pub(crate) const HELP_MEMORY_RECALL: &str =
    "Dry-run memory retrieval for an instruction and show recall provenance.";

pub(crate) const HELP_MEMORY_TRAJECTORY_LIST: &str = "List saved trajectories.";
pub(crate) const HELP_MEMORY_TRAJECTORY_SHOW: &str = "Show one saved trajectory.";
pub(crate) const HELP_MEMORY_TRAJECTORY_REVIEW: &str =
    "Mark a trajectory ready for export after review.";
pub(crate) const HELP_MEMORY_TRAJECTORY_EXPORT: &str = "Export a reviewed trajectory.";

pub(crate) const HELP_PROCEDURE_LIST: &str = "List saved procedures.";
pub(crate) const HELP_PROCEDURE_SHOW: &str = "Show one saved procedure.";
pub(crate) const HELP_PROCEDURE_PRUNE: &str = "Remove superseded and disabled procedures.";
pub(crate) const HELP_PROCEDURE_DISABLE: &str = "Mark a procedure disabled.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TelegramCommandKind {
    Start,
    Help,
    Stop,
    Approvals,
    Approve,
    Deny,
    Access,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TelegramCommandSpec {
    pub(crate) kind: TelegramCommandKind,
    pub(crate) command: &'static str,
    pub(crate) arguments: &'static str,
    pub(crate) description: &'static str,
}

impl TelegramCommandSpec {
    pub(crate) fn usage(self) -> String {
        if self.arguments.is_empty() {
            self.command.to_string()
        } else {
            format!("{} {}", self.command, self.arguments)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedTelegramCommand<'a> {
    pub(crate) kind: TelegramCommandKind,
    pub(crate) argument: &'a str,
}

pub(crate) const TELEGRAM_COMMANDS: &[TelegramCommandSpec] = &[
    TelegramCommandSpec {
        kind: TelegramCommandKind::Start,
        command: "/start",
        arguments: "",
        description: "show configuration and help",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Help,
        command: "/help",
        arguments: "",
        description: "show this message",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Stop,
        command: "/stop",
        arguments: "",
        description: "stop the current task",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Approvals,
        command: "/approvals",
        arguments: "",
        description: "list pending approvals for this chat",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Approve,
        command: "/approve",
        arguments: "<id>",
        description: "approve a pending action",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Deny,
        command: "/deny",
        arguments: "<id>",
        description: "deny a pending action",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Access,
        command: "/access",
        arguments: "[status|set|grant|revoke|audit|lockdown]",
        description: "inspect or change access profile and grants",
    },
    TelegramCommandSpec {
        kind: TelegramCommandKind::Reset,
        command: "/reset",
        arguments: "",
        description: "clear this chat's saved transcript",
    },
];

pub(crate) fn parse_telegram_command(text: &str) -> Option<ParsedTelegramCommand<'_>> {
    let text = text.trim();
    for spec in TELEGRAM_COMMANDS {
        if text == spec.command {
            return Some(ParsedTelegramCommand {
                kind: spec.kind,
                argument: "",
            });
        }
        if let Some(rest) = text.strip_prefix(spec.command) {
            if rest.starts_with(char::is_whitespace) {
                return Some(ParsedTelegramCommand {
                    kind: spec.kind,
                    argument: rest.trim(),
                });
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LifecycleLane {
    pub(crate) name: &'static str,
    pub(crate) source_of_truth_command: &'static str,
    pub(crate) owns: &'static str,
}

pub(crate) const LIFECYCLE_LANES: &[LifecycleLane] = &[
    LifecycleLane {
        name: "runtime contract",
        source_of_truth_command: "topagent config inspect",
        owns: "provider, effective model, key presence, workspace, Telegram admission, runtime options",
    },
    LifecycleLane {
        name: "install/service health",
        source_of_truth_command: "topagent status",
        owns: "installation presence, managed service files, service enabled/running state, configured default model",
    },
    LifecycleLane {
        name: "run snapshots",
        source_of_truth_command: "topagent run status",
        owns: "latest run snapshot, transcript count, and restore guidance",
    },
    LifecycleLane {
        name: "access control",
        source_of_truth_command: "topagent access status",
        owns: "active access profile, network/computer defaults, grants, lockdown state, and audit visibility",
    },
    LifecycleLane {
        name: "workspace learning",
        source_of_truth_command: "topagent memory status",
        owns: "workspace schema, operator model, memory index, notes, procedures, trajectories, exports",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_telegram_command_accepts_declared_commands_only() {
        assert_eq!(
            parse_telegram_command("/help").unwrap().kind,
            TelegramCommandKind::Help
        );
        assert_eq!(
            parse_telegram_command("/approve abc").unwrap(),
            ParsedTelegramCommand {
                kind: TelegramCommandKind::Approve,
                argument: "abc"
            }
        );
        assert!(parse_telegram_command("/approveabc").is_none());
        assert!(parse_telegram_command("/unknown").is_none());
    }

    #[test]
    fn test_lifecycle_lanes_have_single_source_of_truth_commands() {
        let commands = LIFECYCLE_LANES
            .iter()
            .map(|lane| lane.source_of_truth_command)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(commands.len(), LIFECYCLE_LANES.len());
        assert!(commands.contains("topagent config inspect"));
        assert!(commands.contains("topagent status"));
        assert!(commands.contains("topagent run status"));
        assert!(commands.contains("topagent memory status"));
        assert!(commands.contains("topagent access status"));

        let names = LIFECYCLE_LANES
            .iter()
            .map(|lane| lane.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "runtime contract",
                "install/service health",
                "run snapshots",
                "access control",
                "workspace learning",
            ]
        );
    }
}

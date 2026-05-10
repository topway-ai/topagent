use crate::behavior::{BashCommandClass, BehaviorContract};

pub(crate) fn classify_bash_command(command: &str) -> BashCommandClass {
    BehaviorContract::default().classify_bash_command(command)
}

pub(crate) fn is_release_gate_verification_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "scripts/ci-gate.sh",
        "./scripts/ci-gate.sh",
        "cargo test",
        "cargo clippy",
        "cargo build",
        "cargo fmt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn is_meaningful_verification_command(command: &str) -> bool {
    classify_bash_command(command) == BashCommandClass::Verification
}

pub(crate) fn bash_command_family(command: &str) -> String {
    let normalized = command.trim().to_ascii_lowercase();
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        [] => String::new(),
        ["./scripts/ci-gate.sh" | "scripts/ci-gate.sh", ..] => "scripts/ci-gate.sh".to_string(),
        ["cargo", subcommand, ..] => format!("cargo {subcommand}"),
        ["npm", "run", subcommand, ..] => format!("npm run {subcommand}"),
        ["npm", subcommand, ..] => format!("npm {subcommand}"),
        ["pnpm", subcommand, ..] => format!("pnpm {subcommand}"),
        ["yarn", subcommand, ..] => format!("yarn {subcommand}"),
        ["go", subcommand, ..] => format!("go {subcommand}"),
        ["make", target, ..] => format!("make {target}"),
        ["git", subcommand, ..] => format!("git {subcommand}"),
        [command, ..] => (*command).to_string(),
    }
}

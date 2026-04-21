use super::{BashCommandClass, BehaviorContract, MutationPolicy, ToolPolicy};

pub(super) fn default_tool_policy() -> ToolPolicy {
    ToolPolicy {
        repo_awareness_tools: &["git_status", "git_branch", "git_diff"],
        planning_tools: &["update_plan"],
        memory_write_tools: &["save_note", "manage_operator_preference"],
        generated_tool_authoring_tools: &[
            "create_tool",
            "repair_tool",
            "list_generated_tools",
            "delete_generated_tool",
        ],
        research_safe_bash_prefixes: &[
            "cd ",
            "pushd ",
            "popd",
            "ls ",
            "ls-",
            "pwd",
            "find ",
            "find -",
            "rg ",
            "rg -",
            "grep ",
            "grep -",
            "cat ",
            "head ",
            "tail ",
            "wc ",
            "cut ",
            "sort ",
            "uniq ",
            "diff ",
            "git status",
            "git diff",
            "git log ",
            "git show",
            "git blame",
            "git branch",
            "git remote",
            "git stash list",
            "echo ",
            "printf ",
            "true",
            "false",
            "curl ",
            "curl -",
            "http ",
            "httpie ",
            "https ",
        ],
        verification_bash_prefixes: &[
            "cargo test",
            "cargo build",
            "cargo check",
            "cargo clippy",
            "cargo fmt",
            "cargo watch",
            "cargo auditable",
            "cargo deny",
            "cargo audit",
            "pytest",
            "py.test",
            "make test",
            "make check",
            "make verify",
            "npm test",
            "npm run test",
            "npm run build",
            "npm run check",
            "go test",
            "go build",
            "go vet",
            "rustfmt",
            "rust-analyzer",
            "clippy",
            "deny ",
            "audit ",
        ],
        verification_bash_keywords: &["test", "build", "check", "lint", "fmt", "audit", "vet"],
    }
}

pub(super) fn default_mutation_policy() -> MutationPolicy {
    MutationPolicy {
        mutation_tools: &["write", "edit", "git_commit", "git_add"],
        generated_tool_surface_tools: &["create_tool", "repair_tool", "delete_generated_tool"],
        destructive_shell_tokens: &[
            "rm ",
            "mv ",
            "cp ",
            "touch ",
            "mkdir ",
            "curl -o",
            "curl --output",
            "curl --remote-name",
        ],
        shell_write_tokens: &[" >", ">>", "|"],
    }
}

impl ToolPolicy {
    fn is_research_safe_command(&self, cmd: &str) -> bool {
        let lower = cmd.trim().to_lowercase();
        self.research_safe_bash_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix) || lower == prefix.trim_end_matches(' '))
    }

    fn split_shell_segments<'a>(&self, cmd: &'a str) -> Vec<&'a str> {
        let mut segments = Vec::new();
        let mut start = 0;
        let mut chars = cmd.char_indices().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some((idx, ch)) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' if !in_single => escaped = true,
                '\'' if !in_double => in_single = !in_single,
                '"' if !in_single => in_double = !in_double,
                ';' if !in_single && !in_double => {
                    let segment = cmd[start..idx].trim();
                    if !segment.is_empty() {
                        segments.push(segment);
                    }
                    start = idx + ch.len_utf8();
                }
                '|' if !in_single && !in_double => {
                    let is_double_pipe = chars.peek().is_some_and(|(_, next)| *next == '|');
                    if is_double_pipe {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        let (_, next) = chars.next().expect("peeked pipe should exist");
                        start = idx + ch.len_utf8() + next.len_utf8();
                    } else {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        start = idx + ch.len_utf8();
                    }
                }
                '&' if !in_single && !in_double => {
                    if chars.peek().is_some_and(|(_, next)| *next == '&') {
                        let segment = cmd[start..idx].trim();
                        if !segment.is_empty() {
                            segments.push(segment);
                        }
                        let (_, next) = chars.next().expect("peeked ampersand should exist");
                        start = idx + ch.len_utf8() + next.len_utf8();
                    }
                }
                _ => {}
            }
        }

        let tail = cmd[start..].trim();
        if !tail.is_empty() {
            segments.push(tail);
        }

        segments
    }

    pub(crate) fn is_verification_command(&self, cmd: &str) -> bool {
        let lower = cmd.to_lowercase();

        if self
            .verification_bash_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        {
            return true;
        }

        if lower.contains(" --verify") || lower.contains(" --check") {
            return true;
        }

        if lower.ends_with(" --test") || lower.ends_with(" --tests") {
            return true;
        }

        if lower.contains("verify") || lower.contains("lint") && !lower.contains("git") {
            return self
                .verification_bash_keywords
                .iter()
                .any(|indicator| lower.contains(indicator));
        }

        false
    }

    pub(crate) fn is_planning_tool(&self, name: &str) -> bool {
        self.planning_tools.contains(&name)
    }

    pub(crate) fn is_memory_write_tool(&self, name: &str) -> bool {
        self.memory_write_tools.contains(&name)
    }

    pub(crate) fn is_generated_tool_authoring_tool(&self, name: &str) -> bool {
        self.generated_tool_authoring_tools.contains(&name)
    }

    fn classify_bash_command(&self, mutation: &MutationPolicy, cmd: &str) -> BashCommandClass {
        let trimmed = cmd.trim();

        if self.is_verification_command(trimmed) {
            return BashCommandClass::Verification;
        }

        let mut saw_verification = false;
        for segment in self.split_shell_segments(trimmed) {
            if mutation.contains_mutation_signal(segment) {
                return BashCommandClass::MutationRisk;
            }

            if self.is_verification_command(segment) {
                saw_verification = true;
                continue;
            }

            if self.is_research_safe_command(segment) {
                continue;
            }

            return BashCommandClass::MutationRisk;
        }

        if saw_verification {
            BashCommandClass::Verification
        } else {
            BashCommandClass::ResearchSafe
        }
    }
}

impl MutationPolicy {
    fn has_file_write_redirection(&self, cmd: &str) -> bool {
        let mut chars = cmd.char_indices().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some((_, ch)) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' if !in_single => escaped = true,
                '\'' if !in_double => in_single = !in_single,
                '"' if !in_single => in_double = !in_double,
                '>' if !in_single && !in_double => {
                    if chars.peek().is_some_and(|(_, next)| *next == '>') {
                        chars.next();
                    }

                    while chars.peek().is_some_and(|(_, next)| next.is_whitespace()) {
                        chars.next();
                    }

                    let mut target = String::new();
                    while let Some((_, next)) = chars.peek() {
                        if next.is_whitespace() || matches!(next, '|' | ';') {
                            break;
                        }
                        target.push(*next);
                        chars.next();
                    }

                    if target.is_empty() || target.starts_with('&') || target == "/dev/null" {
                        continue;
                    }

                    return true;
                }
                _ => {}
            }
        }

        false
    }

    fn contains_mutation_signal(&self, cmd: &str) -> bool {
        let lower = cmd.trim().to_lowercase();
        self.has_file_write_redirection(cmd)
            || self
                .destructive_shell_tokens
                .iter()
                .any(|token| lower.contains(token))
            || lower.contains(" -delete")
    }

    pub(crate) fn is_mutation_tool(&self, name: &str) -> bool {
        self.mutation_tools.contains(&name)
    }

    pub(crate) fn mutates_generated_tool_surface(&self, name: &str) -> bool {
        self.generated_tool_surface_tools.contains(&name)
    }
}

impl BehaviorContract {
    pub fn classify_bash_command(&self, cmd: &str) -> BashCommandClass {
        self.tools.classify_bash_command(&self.mutation, cmd)
    }

    pub fn is_verification_command(&self, cmd: &str) -> bool {
        self.tools.is_verification_command(cmd)
    }

    pub fn is_planning_tool(&self, name: &str) -> bool {
        self.tools.is_planning_tool(name)
    }

    pub fn is_mutation_tool(&self, name: &str) -> bool {
        self.mutation.is_mutation_tool(name)
    }

    pub fn is_memory_write_tool(&self, name: &str) -> bool {
        self.tools.is_memory_write_tool(name)
    }

    pub fn is_generated_tool_authoring_tool(&self, name: &str) -> bool {
        self.tools.is_generated_tool_authoring_tool(name)
    }

    pub fn mutates_generated_tool_surface(&self, name: &str) -> bool {
        self.mutation.mutates_generated_tool_surface(name)
    }
}

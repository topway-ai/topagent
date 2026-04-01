/// Runtime secret redaction.
///
/// Provides two layers of defense:
/// 1. Known-value redaction — exact matches of registered secret values
/// 2. Pattern-based redaction — regex matches for common secret formats
///
/// Applied to tool output before it enters the model context, and again
/// as a last-mile filter before sending replies to Telegram.
use once_cell::sync::Lazy;
use regex::Regex;
use std::borrow::Cow;
use tracing::warn;

const REDACTED: &str = "[REDACTED_SECRET]";

/// Env var names that must be stripped from child process environments.
/// Prevents the model from reading secrets via `echo $VAR` or `env`.
pub const SECRET_ENV_VARS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "TELEGRAM_BOT_TOKEN",
    // Belt-and-suspenders: catch common alternative names
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "API_KEY",
    "BOT_TOKEN",
];

/// File paths (suffixes) that the bash tool should refuse to read.
/// These are known secret-bearing config files written by `topagent install`.
const SECRET_FILE_SUFFIXES: &[&str] = &[
    "topagent/services/topagent-telegram.env",
    "topagent-telegram.env",
];

/// Bash commands that dump environment variables.
const ENV_DUMP_COMMANDS: &[&str] = &["env", "printenv", "export", "set"];

// ── Pattern-based redaction ──

// Telegram bot token: digits:base64 (e.g. 123456:ABCdef...)
static TELEGRAM_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d{8,}:[A-Za-z0-9_-]{20,}\b").unwrap());

// OpenRouter / OpenAI style API key: sk-or-... or sk-...
static SK_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\bsk-(?:or-)?[A-Za-z0-9_-]{20,}\b").unwrap());

// Generic KEY=value, TOKEN=value, SECRET=value in shell output
static KEY_VALUE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)((?:API_?KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTH)[=:]\s*)(\S+)").unwrap()
});

/// Holds registered secret values for exact-match redaction.
#[derive(Debug, Clone, Default)]
pub struct SecretRegistry {
    values: Vec<String>,
}

impl SecretRegistry {
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Register a secret value. Short or empty values are ignored to avoid
    /// false-positive redaction of common substrings.
    pub fn register(&mut self, value: impl Into<String>) {
        let v = value.into();
        if v.trim().len() < 8 {
            return;
        }
        if !self.values.contains(&v) {
            self.values.push(v);
        }
    }

    /// Redact all registered secrets and pattern-matched secrets from text.
    /// Returns `Cow::Borrowed` when no redaction was needed (zero-alloc fast path).
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if text.is_empty() {
            return Cow::Borrowed(text);
        }

        let mut result = Cow::Borrowed(text);

        // Layer 1: exact value replacement (highest priority)
        for secret in &self.values {
            if result.contains(secret.as_str()) {
                result = Cow::Owned(result.replace(secret.as_str(), REDACTED));
            }
        }

        // Layer 2: pattern-based redaction (only allocates when a pattern matches)
        if let Cow::Owned(s) = TELEGRAM_TOKEN_RE.replace_all(&result, REDACTED) {
            result = Cow::Owned(s);
        }
        if let Cow::Owned(s) = SK_KEY_RE.replace_all(&result, REDACTED) {
            result = Cow::Owned(s);
        }
        if let Cow::Owned(s) = KEY_VALUE_RE.replace_all(&result, |caps: &regex::Captures| {
            format!("{}{}", &caps[1], REDACTED)
        }) {
            result = Cow::Owned(s);
        }

        result
    }
}

/// Check whether `needle` appears as a standalone command in `haystack`,
/// i.e. at the start or after a shell separator (|, ;, &&).
fn contains_shell_command(haystack: &str, needle: &str) -> bool {
    haystack == needle
        || haystack.starts_with(needle)
            && haystack
                .as_bytes()
                .get(needle.len())
                .is_some_and(|&b| b == b' ' || b == b'\n')
        || haystack.contains(&["| ", needle].concat())
        || haystack.contains(&["|", needle].concat())
        || haystack.contains(&["; ", needle].concat())
        || haystack.contains(&[";", needle].concat())
        || haystack.contains(&["&& ", needle].concat())
}

/// Precomputed patterns for secret variable references ($VAR and ${VAR}).
struct SecretVarPatterns {
    dollar: String,
    braced: String,
    name: &'static str,
}

static SECRET_VAR_PATTERNS: Lazy<Vec<SecretVarPatterns>> = Lazy::new(|| {
    SECRET_ENV_VARS
        .iter()
        .map(|&name| SecretVarPatterns {
            dollar: format!("${name}"),
            braced: format!("${{{name}}}"),
            name,
        })
        .collect()
});

/// Check if a bash command is attempting to access secrets.
/// Returns an error message if blocked, None if allowed.
pub fn check_bash_secret_access(command: &str) -> Option<String> {
    let trimmed = command.trim();

    // Block env-dump commands
    for cmd in ENV_DUMP_COMMANDS {
        if contains_shell_command(trimmed, cmd) {
            let msg = format!(
                "Blocked: `{cmd}` dumps environment variables which may contain secrets. \
                 Use specific, non-secret environment variables directly instead."
            );
            warn!("secret access blocked: env-dump command `{cmd}` in: {trimmed}");
            return Some(msg);
        }
    }

    // Block reading known secret files
    for suffix in SECRET_FILE_SUFFIXES {
        if trimmed.contains(suffix) {
            let msg = format!(
                "Blocked: this command references a secret-bearing config file ({suffix}). \
                 Use `topagent status` for safe diagnostics instead."
            );
            warn!("secret access blocked: secret file `{suffix}` in: {trimmed}");
            return Some(msg);
        }
    }

    // Block reading /proc/self/environ
    if trimmed.contains("/proc/self/environ") || trimmed.contains("/proc/*/environ") {
        warn!("secret access blocked: /proc environ access in: {trimmed}");
        return Some("Blocked: /proc/*/environ contains raw environment secrets.".to_string());
    }

    // Block references to known secret variable names
    for pat in SECRET_VAR_PATTERNS.iter() {
        if trimmed.contains(&pat.dollar) || trimmed.contains(&pat.braced) {
            let name = pat.name;
            let msg = format!(
                "Blocked: command references secret variable {name}. \
                 Credentials are configured but cannot be read from chat."
            );
            warn!("secret access blocked: secret var ${name} in: {trimmed}");
            return Some(msg);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_known_secret_value() {
        let mut reg = SecretRegistry::new();
        reg.register("sk-or-v1-abc123def456xyz789");
        let input = "The key is sk-or-v1-abc123def456xyz789 and more text";
        let output = reg.redact(input);
        assert!(
            !output.contains("abc123def456xyz789"),
            "secret should be redacted: {output}"
        );
        assert!(output.contains(REDACTED));
        assert!(output.contains("and more text"));
    }

    #[test]
    fn test_redact_telegram_token_pattern() {
        let reg = SecretRegistry::new();
        let input = "token is 12345678:ABCdefGHIjklMNO_pqrst";
        let output = reg.redact(input);
        assert!(
            !output.contains("ABCdefGHI"),
            "telegram token should be redacted: {output}"
        );
        assert!(output.contains(REDACTED));
    }

    #[test]
    fn test_redact_sk_key_pattern() {
        let reg = SecretRegistry::new();
        let input = "api key: sk-or-v1-abcdefghij1234567890";
        let output = reg.redact(input);
        assert!(
            !output.contains("abcdefghij"),
            "sk key should be redacted: {output}"
        );
    }

    #[test]
    fn test_redact_key_value_pattern() {
        let reg = SecretRegistry::new();
        let input = "OPENROUTER_API_KEY=sk-or-something-long-here";
        let output = reg.redact(input);
        assert!(
            !output.contains("sk-or-something"),
            "key=value should be redacted: {output}"
        );
    }

    #[test]
    fn test_redact_preserves_safe_text() {
        let reg = SecretRegistry::new();
        let input = "The workspace is /home/user/project and the service is running.";
        let output = reg.redact(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_short_secrets_not_registered() {
        let mut reg = SecretRegistry::new();
        reg.register("short");
        assert!(reg.values.is_empty());
    }

    #[test]
    fn test_empty_secrets_not_registered() {
        let mut reg = SecretRegistry::new();
        reg.register("");
        reg.register("   ");
        assert!(reg.values.is_empty());
    }

    #[test]
    fn test_check_bash_env_dump_blocked() {
        assert!(check_bash_secret_access("env").is_some());
        assert!(check_bash_secret_access("printenv").is_some());
        assert!(check_bash_secret_access("export").is_some());
        assert!(check_bash_secret_access("cat file | env").is_some());
    }

    #[test]
    fn test_check_bash_secret_file_blocked() {
        assert!(
            check_bash_secret_access("cat ~/.config/topagent/services/topagent-telegram.env")
                .is_some()
        );
        assert!(check_bash_secret_access("grep token topagent-telegram.env").is_some());
    }

    #[test]
    fn test_check_bash_secret_var_reference_blocked() {
        assert!(check_bash_secret_access("echo $OPENROUTER_API_KEY").is_some());
        assert!(check_bash_secret_access("echo ${TELEGRAM_BOT_TOKEN}").is_some());
    }

    #[test]
    fn test_check_bash_proc_environ_blocked() {
        assert!(check_bash_secret_access("cat /proc/self/environ").is_some());
    }

    #[test]
    fn test_check_bash_safe_commands_allowed() {
        assert!(check_bash_secret_access("ls -la").is_none());
        assert!(check_bash_secret_access("git status").is_none());
        assert!(check_bash_secret_access("cargo build").is_none());
        assert!(check_bash_secret_access("echo hello").is_none());
        assert!(check_bash_secret_access("cat src/main.rs").is_none());
        assert!(check_bash_secret_access("grep TODO src/").is_none());
    }

    #[test]
    fn test_redact_multiple_secrets_in_one_string() {
        let mut reg = SecretRegistry::new();
        reg.register("12345678:ABCdefGHIjklMNOpqrstuv");
        reg.register("sk-or-v1-abc123def456xyz789000");
        let input = "token=12345678:ABCdefGHIjklMNOpqrstuv key=sk-or-v1-abc123def456xyz789000";
        let output = reg.redact(input);
        assert!(!output.contains("ABCdef"), "token should be redacted");
        assert!(!output.contains("abc123"), "key should be redacted");
    }

    #[test]
    fn test_check_bash_env_subcommands_allowed() {
        // "environment" as a word in a normal command should not trigger
        assert!(check_bash_secret_access("echo environment").is_none());
        // "envsubst" is a different command
        assert!(check_bash_secret_access("envsubst < template").is_none());
    }
}

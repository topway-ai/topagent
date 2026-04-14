use crate::command_exec::{CommandSandboxPolicy, run_command};
use crate::context::ToolContext;
use crate::file_util::format_command_output_with_limit;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalToolEffect {
    #[default]
    ReadOnly,
    VerificationOnly,
    ExecutionStarted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalToolResult {
    pub output: String,
    pub effect: ExternalToolEffect,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalToolConfig {
    pub name: String,
    pub description: String,
    pub command: String,
    pub argv_template: Vec<String>,
    pub sandbox: CommandSandboxPolicy,
    #[serde(default)]
    pub effect: ExternalToolEffect,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceExternalToolConfig {
    name: String,
    description: String,
    command: String,
    argv_template: Vec<String>,
    sandbox: CommandSandboxPolicy,
    #[serde(default)]
    effect: ExternalToolEffect,
}

#[derive(Debug, Clone)]
pub struct ExternalTool {
    config: ExternalToolConfig,
    input_schema: serde_json::Value,
}

impl ExternalTool {
    pub fn new(name: &str, description: &str, command: &str) -> Self {
        Self {
            config: ExternalToolConfig {
                name: name.to_string(),
                description: description.to_string(),
                command: command.to_string(),
                argv_template: Vec::new(),
                sandbox: CommandSandboxPolicy::Host,
                effect: ExternalToolEffect::ReadOnly,
            },
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    pub fn with_description(mut self, description: &str) -> Self {
        self.config.description = description.to_string();
        self
    }

    pub fn with_input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn with_command(mut self, command: &str) -> Self {
        self.config.command = command.to_string();
        self
    }

    pub fn with_argv_template(mut self, argv: Vec<String>) -> Self {
        self.config.argv_template = argv;
        self
    }

    pub fn with_effect(mut self, effect: ExternalToolEffect) -> Self {
        self.config.effect = effect;
        self
    }

    pub fn with_sandbox_policy(mut self, sandbox: CommandSandboxPolicy) -> Self {
        self.config.sandbox = sandbox;
        self
    }

    pub fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.config.name.clone(),
            description: format!(
                "{} ({})",
                self.config.description,
                self.config.sandbox.description_suffix()
            ),
            input_schema: self.input_schema.clone(),
        }
    }

    pub fn effect(&self) -> ExternalToolEffect {
        self.config.effect
    }

    pub fn sandbox_policy(&self) -> CommandSandboxPolicy {
        self.config.sandbox
    }

    fn from_config(config: ExternalToolConfig) -> Result<Self> {
        Ok(Self {
            input_schema: build_input_schema_from_argv_template(&config.argv_template),
            config,
        })
    }

    pub fn execute(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ExternalToolResult> {
        let resolved_argv =
            resolve_argv_template(&self.config.argv_template, args, &self.config.name)?;

        let display_name = format!("external tool '{}'", self.config.name);
        let output = run_command(
            &self.config.command,
            &resolved_argv,
            &ctx.exec.workspace_root,
            ctx.exec.cancel_token(),
            self.config.sandbox,
            &display_name,
        )?;

        if !output.status.success() {
            let summary =
                format_command_output_with_limit(output, ctx.runtime.max_bash_output_bytes);
            return Err(Error::ToolFailed(format!(
                "external tool '{}' failed\n{}",
                self.config.name, summary
            )));
        }

        Ok(ExternalToolResult {
            output: format_command_output_with_limit(output, ctx.runtime.max_bash_output_bytes),
            effect: self.config.effect,
        })
    }
}

pub struct ExternalToolRegistry {
    tools: HashMap<String, ExternalTool>,
}

impl ExternalToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: ExternalTool) {
        self.tools.insert(tool.config.name.clone(), tool);
    }

    pub fn remove(&mut self, name: &str) -> Option<ExternalTool> {
        self.tools.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&ExternalTool> {
        self.tools.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<_> = self.tools.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<_> = self.tools.values().map(|t| t.spec()).collect();
        specs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        specs
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn load_from_str(&mut self, content: &str) -> Result<()> {
        let configs: Vec<WorkspaceExternalToolConfig> =
            serde_json::from_str(content).map_err(|e| {
                Error::InvalidInput(format!("failed to parse external tools JSON: {}", e))
            })?;
        for config in configs {
            let tool = ExternalTool::from_config(config.into_external_tool_config())?;
            self.register(tool);
        }
        Ok(())
    }
}

impl Default for ExternalToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceExternalToolConfig {
    fn into_external_tool_config(self) -> ExternalToolConfig {
        ExternalToolConfig {
            name: self.name,
            description: self.description,
            command: self.command,
            argv_template: self.argv_template,
            sandbox: self.sandbox,
            effect: self.effect,
        }
    }
}

pub(crate) fn resolve_argv_template(
    argv_template: &[String],
    args: &serde_json::Value,
    tool_name: &str,
) -> Result<Vec<String>> {
    let placeholders = placeholder_names(argv_template);

    if let Some(obj) = args.as_object() {
        for key in obj.keys() {
            if !placeholders.contains(&key.as_str()) {
                return Err(Error::InvalidInput(format!(
                    "unknown input '{}' for tool '{}'",
                    key, tool_name
                )));
            }
        }
    }

    for placeholder in &placeholders {
        if args.get(*placeholder).is_none() {
            return Err(Error::InvalidInput(format!(
                "missing required input '{}' for tool '{}'",
                placeholder, tool_name
            )));
        }
    }

    let mut resolved = Vec::with_capacity(argv_template.len());
    for part in argv_template {
        if part.starts_with('{') && part.ends_with('}') {
            let key = &part[1..part.len() - 1];
            let value = args.get(key).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "missing required input '{}' for tool '{}'",
                    key, tool_name
                ))
            })?;
            let value = value.as_str().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "input '{}' for tool '{}' must be a string",
                    key, tool_name
                ))
            })?;
            resolved.push(value.to_string());
        } else {
            resolved.push(part.clone());
        }
    }

    Ok(resolved)
}

fn placeholder_names(argv_template: &[String]) -> Vec<&str> {
    argv_template
        .iter()
        .filter_map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                Some(&part[1..part.len() - 1])
            } else {
                None
            }
        })
        .collect()
}

fn build_input_schema_from_argv_template(argv_template: &[String]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = BTreeSet::new();

    for placeholder in placeholder_names(argv_template) {
        properties
            .entry(placeholder.to_string())
            .or_insert_with(|| {
                serde_json::json!({
                    "type": "string",
                    "description": format!("value for {{{}}}", placeholder)
                })
            });
        required.insert(placeholder.to_string());
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required.into_iter().collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::CancellationToken;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_external_tool_with_structured_argv() {
        let tool = ExternalTool::new("echo", "echo tool", "echo").with_argv_template(vec![
            "Hello,".to_string(),
            "{name}".to_string(),
            "!".to_string(),
        ]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"name": "World"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.effect, ExternalToolEffect::ReadOnly);
        assert!(output.output.contains("Hello,"));
        assert!(output.output.contains("World"));
        assert!(output.output.contains("!"));
    }

    #[test]
    fn test_external_tool_structured_argv_preserves_spaces() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{msg}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": "hello world with spaces"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap().output;
        assert!(output.contains("hello world with spaces"));
    }

    #[test]
    fn test_external_tool_structured_argv_special_chars() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{input}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"input": "foo --bar=baz \"qux\""}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap().output;
        assert!(output.contains("foo --bar=baz \"qux\""));
    }

    #[test]
    fn test_external_tool_structured_argv_no_extra_shell_parsing() {
        let tool = ExternalTool::new("printf", "printf tool", "printf")
            .with_argv_template(vec!["%s\\n".to_string(), "{arg}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"arg": "$HOME"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap().output;
        assert!(output.contains("$HOME"));
        assert!(!output.contains("/home"));
    }

    #[test]
    fn test_external_tool_fails_clearly() {
        let tool = ExternalTool::new("false", "fails", "false").with_argv_template(vec![]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_external_tool_missing_required_input_fails() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{msg}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("missing required input 'msg'"));
    }

    #[test]
    fn test_external_tool_unknown_input_key_fails() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{msg}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": "hello", "unknown": "key"}), &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unknown input 'unknown'"));
    }

    #[test]
    fn test_external_tool_non_string_input_fails() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{msg}".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": 123}), &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("must be a string"));
    }

    #[test]
    fn test_external_tool_empty_argv_template_runs_without_inputs() {
        let tool = ExternalTool::new("echo", "echo tool", "echo");

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_external_tool_registry() {
        let mut registry = ExternalToolRegistry::new();
        let tool = ExternalTool::new("test", "test tool", "true").with_argv_template(vec![]);
        registry.register(tool);

        assert!(registry.get("test").is_some());
        assert_eq!(registry.names(), vec!["test"]);
    }

    #[test]
    fn test_external_tool_registry_load_from_str() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "tool1", "description": "first tool", "command": "echo", "argv_template": ["hello"], "sandbox": "host"},
            {"name": "tool2", "description": "second tool", "command": "ls", "argv_template": ["{path}", "-la"], "sandbox": "workspace", "effect": "execution_started"}
        ]"#;
        registry.load_from_str(json).unwrap();

        assert!(registry.get("tool1").is_some());
        assert!(registry.get("tool2").is_some());
        assert_eq!(
            registry.get("tool1").unwrap().effect(),
            ExternalToolEffect::ReadOnly
        );
        assert_eq!(
            registry.get("tool2").unwrap().effect(),
            ExternalToolEffect::ExecutionStarted
        );
        let specs = registry.specs();
        assert_eq!(specs[0].name, "tool1");
        assert_eq!(specs[1].name, "tool2");
        assert_eq!(
            specs[1].input_schema["required"],
            serde_json::json!(["path"])
        );
        assert_eq!(
            registry.get("tool1").unwrap().sandbox_policy(),
            CommandSandboxPolicy::Host
        );
        assert_eq!(
            registry.get("tool2").unwrap().sandbox_policy(),
            CommandSandboxPolicy::Workspace
        );
    }

    #[test]
    fn test_external_tool_registry_supports_explicit_host_sandbox() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "host_tool", "description": "host tool", "command": "echo", "argv_template": ["hello", "{name}"], "sandbox": "host"}
        ]"#;

        registry.load_from_str(json).unwrap();

        let tool = registry.get("host_tool").unwrap();
        assert_eq!(
            tool.spec().input_schema["required"],
            serde_json::json!(["name"])
        );
        assert_eq!(tool.sandbox_policy(), CommandSandboxPolicy::Host);
        assert!(tool.spec().description.contains("host execution"));
    }

    #[test]
    fn test_external_tool_registry_supports_workspace_sandbox_opt_in() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "repo_tool", "description": "repo-local", "command": "rg", "argv_template": ["TODO", "{path}"], "sandbox": "workspace"}
        ]"#;

        registry.load_from_str(json).unwrap();

        let tool = registry.get("repo_tool").unwrap();
        assert_eq!(tool.sandbox_policy(), CommandSandboxPolicy::Workspace);
        assert!(tool.spec().description.contains("sandboxed workspace"));
    }

    #[test]
    fn test_external_tool_registry_rejects_missing_argv_template() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "broken", "description": "broken tool", "command": "echo", "sandbox": "host"}
        ]"#;

        let err = registry.load_from_str(json).unwrap_err();
        assert!(err.to_string().contains("missing field `argv_template`"));
    }

    #[test]
    fn test_external_tool_registry_rejects_missing_sandbox() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "broken", "description": "broken tool", "command": "echo", "argv_template": ["hello"]}
        ]"#;

        let err = registry.load_from_str(json).unwrap_err();
        assert!(err.to_string().contains("missing field `sandbox`"));
    }

    #[test]
    fn test_external_tool_effect_builder() {
        let tool = ExternalTool::new("exec", "execution tool", "true")
            .with_argv_template(vec![])
            .with_effect(ExternalToolEffect::ExecutionStarted);
        assert_eq!(tool.effect(), ExternalToolEffect::ExecutionStarted);
    }

    #[test]
    fn test_external_tool_strips_secret_env_from_child_process() {
        let tool = ExternalTool::new("echo_secret", "echo env", "sh").with_argv_template(vec![
            "-c".to_string(),
            "printf %s \"$OPENROUTER_API_KEY\"".to_string(),
        ]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        std::env::set_var("OPENROUTER_API_KEY", "super-secret-openrouter-key");
        let output = tool.execute(&serde_json::json!({}), &ctx).unwrap().output;
        std::env::remove_var("OPENROUTER_API_KEY");

        assert!(
            !output.contains("super-secret-openrouter-key"),
            "external tools must not inherit secret env vars: {output}"
        );
    }

    #[test]
    fn test_external_tool_output_respects_runtime_limit() {
        let tool = ExternalTool::new("long_output", "long output", "sh")
            .with_argv_template(vec!["-c".to_string(), "printf 1234567890".to_string()]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default().with_max_bash_output_bytes(5);
        let ctx = ToolContext::new(&exec, &runtime);
        let output = tool.execute(&serde_json::json!({}), &ctx).unwrap().output;

        assert!(output.contains("Output truncated"));
        assert!(output.contains("showing first 5"));
    }

    #[test]
    fn test_external_tool_honors_cancellation() {
        let tool = ExternalTool::new("sleep", "sleep tool", "sleep")
            .with_argv_template(vec!["30".to_string()]);

        let temp = TempDir::new().unwrap();
        let token = CancellationToken::new();
        let exec =
            ExecutionContext::new(temp.path().to_path_buf()).with_cancel_token(token.clone());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            token.cancel();
        });

        let err = tool.execute(&serde_json::json!({}), &ctx).unwrap_err();
        assert!(matches!(err, Error::Stopped(_)));
    }
}

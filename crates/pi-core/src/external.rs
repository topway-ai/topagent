use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolConfig {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args_template: Option<String>,
    #[serde(default)]
    pub argv_template: Option<Vec<String>>,
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
                args_template: None,
                argv_template: None,
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

    pub fn with_args_template(mut self, template: &str) -> Self {
        self.config.args_template = Some(template.to_string());
        self
    }

    pub fn with_argv_template(mut self, argv: Vec<String>) -> Self {
        self.config.argv_template = Some(argv);
        self
    }

    pub fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.config.name.clone(),
            description: self.config.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }

    pub fn execute(&self, args: &serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let mut cmd = Command::new(&self.config.command);
        cmd.current_dir(&ctx.exec.workspace_root);

        if let Some(ref argv_template) = self.config.argv_template {
            for part in argv_template {
                if part.starts_with('{') && part.ends_with('}') {
                    let key = &part[1..part.len() - 1];
                    if let Some(value) = args.get(key).and_then(|v| v.as_str()) {
                        cmd.arg(value);
                    }
                } else {
                    cmd.arg(part);
                }
            }
        } else if let Some(template) = &self.config.args_template {
            let args_str = substitute_args(template, args);
            cmd.arg(&args_str);
        } else if let Some(obj) = args.as_object() {
            for (_key, value) in obj {
                if let Some(s) = value.as_str() {
                    cmd.arg(s);
                } else {
                    cmd.arg(value.to_string());
                }
            }
        }

        let output = cmd.output().map_err(|e| {
            Error::ToolFailed(format!(
                "failed to execute external tool '{}': {}",
                self.config.name, e
            ))
        })?;

        if !output.status.success() {
            return Err(Error::ToolFailed(format!(
                "external tool '{}' failed with exit code: {:?}",
                self.config.name,
                output.status.code()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("stderr: ");
            result.push_str(&stderr);
        }

        Ok(result)
    }
}

fn substitute_args(template: &str, args: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = args.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = if let Some(s) = value.as_str() {
                s.to_string()
            } else {
                value.to_string()
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
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

    pub fn get(&self, name: &str) -> Option<&ExternalTool> {
        self.tools.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn load_from_str(&mut self, content: &str) -> Result<()> {
        let configs: Vec<ExternalToolConfig> = serde_json::from_str(content).map_err(|e| {
            Error::InvalidInput(format!("failed to parse external tools JSON: {}", e))
        })?;
        for config in configs {
            let tool = ExternalTool {
                config,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, ToolContext};
    use crate::runtime::RuntimeOptions;
    use tempfile::TempDir;

    #[test]
    fn test_external_tool_simple_execution() {
        let tool =
            ExternalTool::new("echo", "echo tool", "echo").with_description("echoes arguments");

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": "hello"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[test]
    fn test_external_tool_with_template() {
        let mut tool = ExternalTool::new("greet", "greeting tool", "echo");
        tool.config.args_template = Some("Hello, {name}!".to_string());

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"name": "World"}), &ctx);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Hello, World!"));
    }

    #[test]
    fn test_external_tool_registry() {
        let mut registry = ExternalToolRegistry::new();
        let tool = ExternalTool::new("test", "test tool", "true");
        registry.register(tool);

        assert!(registry.get("test").is_some());
        assert_eq!(registry.names(), vec!["test"]);
    }

    #[test]
    fn test_external_tool_registry_load_from_str() {
        let mut registry = ExternalToolRegistry::new();
        let json = r#"[
            {"name": "tool1", "description": "first tool", "command": "echo", "args_template": null},
            {"name": "tool2", "description": "second tool", "command": "ls", "args_template": "-la"}
        ]"#;
        registry.load_from_str(json).unwrap();

        assert!(registry.get("tool1").is_some());
        assert!(registry.get("tool2").is_some());
    }

    #[test]
    fn test_external_tool_fails_clearly() {
        let tool = ExternalTool::new("false", "fails", "false");

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

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
        assert!(output.contains("Hello,"));
        assert!(output.contains("World"));
        assert!(output.contains("!"));
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
        let output = result.unwrap();
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
        let output = result.unwrap();
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
        let output = result.unwrap();
        assert!(output.contains("$HOME"));
        assert!(!output.contains("/home"));
    }

    #[test]
    fn test_external_tool_structured_priority_over_legacy() {
        let tool = ExternalTool::new("echo", "echo tool", "echo")
            .with_argv_template(vec!["{msg}".to_string()])
            .with_args_template("LEGACY {msg}");

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({"msg": "structured"}), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("structured"));
        assert!(!output.contains("LEGACY"));
    }

    #[test]
    fn test_external_tool_no_args_still_works() {
        let tool = ExternalTool::new("true", "does nothing", "true").with_argv_template(vec![]);

        let temp = TempDir::new().unwrap();
        let exec = ExecutionContext::new(temp.path().to_path_buf());
        let runtime = RuntimeOptions::default();
        let ctx = ToolContext::new(&exec, &runtime);
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_ok());
    }
}

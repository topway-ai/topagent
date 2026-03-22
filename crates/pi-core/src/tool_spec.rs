#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

impl ToolSpec {
    pub fn read() -> Self {
        Self {
            name: "read",
            description: "read file contents",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        }
    }

    pub fn write() -> Self {
        Self {
            name: "write",
            description: "write file contents",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        }
    }

    pub fn edit() -> Self {
        Self {
            name: "edit",
            description: "replace exact text in a file; fails if target text is absent or ambiguous (unless replace_all is true)",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "relative path to file"},
                    "old_text": {"type": "string", "description": "exact text to find and replace"},
                    "new_text": {"type": "string", "description": "replacement text"},
                    "replace_all": {"type": "boolean", "description": "if true, replace all occurrences; otherwise fails if multiple matches exist", "default": false}
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    pub fn bash() -> Self {
        Self {
            name: "bash",
            description: "execute bash command locally in workspace (trusted local execution)",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "shell command to execute in workspace directory"}
                },
                "required": ["command"]
            }),
        }
    }
}

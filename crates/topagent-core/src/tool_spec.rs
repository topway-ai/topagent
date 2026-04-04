#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ToolSpec {
    pub fn read() -> Self {
        Self {
            name: "read".to_string(),
            description:
                "read text file contents (max 64KB, truncated if larger; binary files rejected)"
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "relative path to text file"}
                },
                "required": ["path"]
            }),
        }
    }

    pub fn write() -> Self {
        Self {
            name: "write".to_string(),
            description: "write file contents".to_string(),
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
            name: "edit".to_string(),
            description: "replace exact text in a file; fails if target is absent or ambiguous (unless replace_all is true)".to_string(),
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
            name: "bash".to_string(),
            description: "execute bash command locally in workspace (workspace sandbox when bwrap is available; network disabled; output truncated at 64KB per stream)".to_string(),
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

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
            description: "replace first occurrence of find string with replace",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "find": {"type": "string"},
                    "replace": {"type": "string"}
                },
                "required": ["path", "find", "replace"]
            }),
        }
    }

    pub fn bash() -> Self {
        Self {
            name: "bash",
            description: "execute bash command",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }
    }
}

pub fn all_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::read(),
        ToolSpec::write(),
        ToolSpec::edit(),
        ToolSpec::bash(),
    ]
}

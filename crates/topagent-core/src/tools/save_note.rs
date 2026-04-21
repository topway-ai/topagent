use crate::context::ToolContext;
use crate::tool_spec::ToolSpec;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const NOTES_DIR: &str = ".topagent/notes";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveNoteArgs {
    pub title: String,
    pub what_changed: String,
    pub what_learned: String,
    pub reuse_next_time: Option<String>,
    pub avoid_next_time: Option<String>,
}

pub struct SaveNoteTool;

impl SaveNoteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SaveNoteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::Tool for SaveNoteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "save_note".to_string(),
            description: "Save a lesson learned note for future reference. Use sparingly - only for genuinely useful lessons that improve future work.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A short descriptive title for this lesson"
                    },
                    "what_changed": {
                        "type": "string",
                        "description": "What was changed or implemented"
                    },
                    "what_learned": {
                        "type": "string",
                        "description": "What was learned from this experience"
                    },
                    "reuse_next_time": {
                        "type": "string",
                        "description": "Optional: What to reuse or do again next time"
                    },
                    "avoid_next_time": {
                        "type": "string",
                        "description": "Optional: What to avoid or do differently next time"
                    }
                },
                "required": ["title", "what_changed", "what_learned"]
            }),
        }
    }

    fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String> {
        let args: SaveNoteArgs = serde_json::from_value(args)
            .map_err(|e| Error::InvalidInput(format!("save_note: invalid input: {}", e)))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::ToolFailed(format!("save_note: time error: {}", e)))?
            .as_secs();

        let slug = args
            .title
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
            .collect::<String>()
            .chars()
            .take(40)
            .collect::<String>()
            .replace(' ', "-");

        let filename = format!("{}-{}.md", timestamp, slug);
        let notes_dir = ctx.exec.workspace_root.join(NOTES_DIR);
        std::fs::create_dir_all(&notes_dir).map_err(|e| {
            Error::ToolFailed(format!("save_note: failed to create directory: {}", e))
        })?;

        let filepath = notes_dir.join(&filename);

        let mut content = String::new();
        content.push_str(&format!("# {}\n\n", args.title));
        content.push_str(&format!("**Saved:** <t:{}>\n\n", timestamp));
        content.push_str("---\n\n");
        content.push_str(&format!("## What Changed\n\n{}\n\n", args.what_changed));
        content.push_str(&format!("## What Was Learned\n\n{}\n\n", args.what_learned));

        if let Some(reuse) = args.reuse_next_time {
            content.push_str(&format!("## Reuse Next Time\n\n{}\n\n", reuse));
        }
        if let Some(avoid) = args.avoid_next_time {
            content.push_str(&format!("## Avoid Next Time\n\n{}\n\n", avoid));
        }

        content.push_str("---\n*Saved by topagent*\n");

        std::fs::write(&filepath, &content)
            .map_err(|e| Error::ToolFailed(format!("save_note: failed to write file: {}", e)))?;

        Ok(format!(
            "Note saved to .topagent/notes/{}\n\n{}",
            filename, content
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use tempfile::TempDir;

    #[test]
    fn test_save_note_creates_file() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let exec = crate::context::ExecutionContext::new(root);
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let tool = SaveNoteTool::new();

        let args = serde_json::json!({
            "title": "Test Lesson",
            "what_changed": "Implemented new feature",
            "what_learned": "Testing is important"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok(), "save_note failed: {:?}", result);
        let output = result.unwrap();
        assert!(output.contains(".topagent/notes/"));
        assert!(output.contains("Test Lesson"));
        assert!(output.contains("Implemented new feature"));
        assert!(output.contains("Testing is important"));
    }

    #[test]
    fn test_save_note_with_optional_fields() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let exec = crate::context::ExecutionContext::new(root);
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let tool = SaveNoteTool::new();

        let args = serde_json::json!({
            "title": "Full Lesson",
            "what_changed": "Made changes",
            "what_learned": "Learned stuff",
            "reuse_next_time": "Do this again",
            "avoid_next_time": "Don't do that"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Reuse Next Time"));
        assert!(output.contains("Avoid Next Time"));
        assert!(output.contains("Do this again"));
        assert!(output.contains("Don't do that"));
    }

    #[test]
    fn test_save_note_minimal() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let exec = crate::context::ExecutionContext::new(root);
        let runtime = crate::runtime::RuntimeOptions::default();
        let ctx = crate::context::ToolContext::new(&exec, &runtime);

        let tool = SaveNoteTool::new();

        let args = serde_json::json!({
            "title": "Minimal Lesson",
            "what_changed": "Changed one thing",
            "what_learned": "Learned one thing"
        });

        let result = tool.execute(args, &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Minimal Lesson"));
        assert!(!output.contains("Reuse Next Time"));
    }
}

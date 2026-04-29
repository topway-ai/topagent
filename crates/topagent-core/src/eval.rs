use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalRunRecord {
    pub task_id: String,
    pub success: bool,
    pub failure: Option<String>,
    pub wall_time_ms: u64,
    pub model_turns: usize,
    pub skill_calls: usize,
    pub approval_blocks: usize,
    pub verification_command: Option<String>,
    pub files_changed: Vec<String>,
}

impl EvalRunRecord {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            success: false,
            failure: None,
            wall_time_ms: 0,
            model_turns: 0,
            skill_calls: 0,
            approval_blocks: 0,
            verification_command: None,
            files_changed: Vec::new(),
        }
    }

    pub fn with_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    pub fn with_failure(mut self, failure: impl Into<String>) -> Self {
        self.success = false;
        self.failure = Some(failure.into());
        self
    }

    pub fn with_wall_time_ms(mut self, wall_time_ms: u64) -> Self {
        self.wall_time_ms = wall_time_ms;
        self
    }

    pub fn with_model_turns(mut self, model_turns: usize) -> Self {
        self.model_turns = model_turns;
        self
    }

    pub fn with_skill_calls(mut self, skill_calls: usize) -> Self {
        self.skill_calls = skill_calls;
        self
    }

    pub fn with_approval_blocks(mut self, approval_blocks: usize) -> Self {
        self.approval_blocks = approval_blocks;
        self
    }

    pub fn with_verification_command(mut self, verification_command: impl Into<String>) -> Self {
        self.verification_command = Some(verification_command.into());
        self
    }

    pub fn with_files_changed(mut self, files_changed: Vec<String>) -> Self {
        self.files_changed = files_changed;
        self
    }
}

#[derive(Debug, Clone)]
pub struct EvalRecorder {
    path: PathBuf,
}

impl EvalRecorder {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &EvalRunRecord) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, record).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_record_captures_required_fields() {
        let record = EvalRunRecord::new("task-1")
            .with_success(true)
            .with_wall_time_ms(123)
            .with_model_turns(4)
            .with_skill_calls(3)
            .with_approval_blocks(1)
            .with_verification_command("cargo test")
            .with_files_changed(vec!["src/lib.rs".to_string()]);

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["success"], true);
        assert_eq!(json["wall_time_ms"], 123);
        assert_eq!(json["model_turns"], 4);
        assert_eq!(json["skill_calls"], 3);
        assert_eq!(json["approval_blocks"], 1);
        assert_eq!(json["verification_command"], "cargo test");
        assert_eq!(json["files_changed"][0], "src/lib.rs");
    }

    #[test]
    fn test_eval_recorder_appends_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("evals").join("runs.jsonl");
        let recorder = EvalRecorder::new(&path);
        recorder.append(&EvalRunRecord::new("task-1")).unwrap();
        recorder
            .append(&EvalRunRecord::new("task-2").with_failure("failed"))
            .unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"task_id\":\"task-1\""));
        assert!(lines[1].contains("\"failure\":\"failed\""));
    }
}

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
    #[serde(default)]
    pub workflow_status: Option<String>,
    #[serde(default)]
    pub run_evidence_status: Option<String>,
    #[serde(default)]
    pub resume_next_action: Option<String>,
    #[serde(default)]
    pub unresolved_risks: Vec<String>,
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
            workflow_status: None,
            run_evidence_status: None,
            resume_next_action: None,
            unresolved_risks: Vec::new(),
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

    pub fn with_workflow_status(mut self, workflow_status: impl Into<String>) -> Self {
        self.workflow_status = Some(workflow_status.into());
        self
    }

    pub fn with_run_evidence_status(mut self, status: impl Into<String>) -> Self {
        self.run_evidence_status = Some(status.into());
        self
    }

    pub fn with_resume_next_action(mut self, action: impl Into<String>) -> Self {
        self.resume_next_action = Some(action.into());
        self
    }

    pub fn with_unresolved_risks(mut self, unresolved_risks: Vec<String>) -> Self {
        self.unresolved_risks = unresolved_risks;
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
            .with_files_changed(vec!["src/lib.rs".to_string()])
            .with_workflow_status("satisfied")
            .with_run_evidence_status("satisfied")
            .with_resume_next_action("review final output")
            .with_unresolved_risks(vec!["blocked external action".to_string()]);

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["success"], true);
        assert_eq!(json["wall_time_ms"], 123);
        assert_eq!(json["model_turns"], 4);
        assert_eq!(json["skill_calls"], 3);
        assert_eq!(json["approval_blocks"], 1);
        assert_eq!(json["verification_command"], "cargo test");
        assert_eq!(json["files_changed"][0], "src/lib.rs");
        assert_eq!(json["workflow_status"], "satisfied");
        assert_eq!(json["run_evidence_status"], "satisfied");
        assert_eq!(json["resume_next_action"], "review final output");
        assert_eq!(json["unresolved_risks"][0], "blocked external action");
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

    #[test]
    fn test_eval_records_workflow_reliability_cases_without_prompt_memory() {
        let cases = vec![
            EvalRunRecord::new("incomplete-plan")
                .with_workflow_status("incomplete")
                .with_run_evidence_status("incomplete")
                .with_resume_next_action("finish or block remaining queue"),
            EvalRunRecord::new("release-gate-verified")
                .with_success(true)
                .with_verification_command("scripts/ci-gate.sh")
                .with_workflow_status("satisfied")
                .with_run_evidence_status("satisfied"),
            EvalRunRecord::new("resume-blocked-approval")
                .with_workflow_status("blocked")
                .with_run_evidence_status("blocked")
                .with_unresolved_risks(vec!["approval required: external_send".to_string()]),
        ];

        let json = serde_json::to_string(&cases).unwrap();

        assert!(json.contains("release-gate-verified"));
        assert!(json.contains("scripts/ci-gate.sh"));
        assert!(json.contains("resume-blocked-approval"));
        assert!(!json.contains("raw_receipts"));
        assert!(!json.contains("transcript"));
    }
}

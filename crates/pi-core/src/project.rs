use crate::Error;
use std::path::Path;

pub const PROJECT_INSTRUCTIONS_FILENAME: &str = "PI.md";

#[derive(Debug)]
pub enum ProjectInstructionResult {
    Missing,
    Loaded(String),
    ReadError(String),
}

pub fn load_project_instructions(workspace_root: &Path) -> Result<ProjectInstructionResult, Error> {
    let pi_path = workspace_root.join(PROJECT_INSTRUCTIONS_FILENAME);

    if !pi_path.exists() {
        return Ok(ProjectInstructionResult::Missing);
    }

    match std::fs::read_to_string(&pi_path) {
        Ok(content) => Ok(ProjectInstructionResult::Loaded(content)),
        Err(e) => Ok(ProjectInstructionResult::ReadError(format!(
            "PI.md exists at {} but could not be read: {}",
            pi_path.display(),
            e
        ))),
    }
}

pub fn get_project_instructions_or_error(workspace_root: &Path) -> Result<Option<String>, Error> {
    match load_project_instructions(workspace_root)? {
        ProjectInstructionResult::Missing => Ok(None),
        ProjectInstructionResult::Loaded(content) => Ok(Some(content)),
        ProjectInstructionResult::ReadError(msg) => Err(Error::ProjectInstruction(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_existing_project_instructions() {
        let temp = TempDir::new().unwrap();
        let pi_content = "# Project Instructions\n\nUse Rust.\n";
        fs::write(temp.path().join(PROJECT_INSTRUCTIONS_FILENAME), pi_content).unwrap();

        let result = load_project_instructions(temp.path()).unwrap();
        match result {
            ProjectInstructionResult::Loaded(content) => {
                assert_eq!(content, pi_content);
            }
            _ => panic!("expected Loaded, got {:?}", result),
        }
    }

    #[test]
    fn test_load_missing_project_instructions() {
        let temp = TempDir::new().unwrap();
        let result = load_project_instructions(temp.path()).unwrap();
        match result {
            ProjectInstructionResult::Missing => {}
            _ => panic!("expected Missing, got {:?}", result),
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_load_unreadable_project_instructions() {
        use std::process::Command;

        let temp = TempDir::new().unwrap();
        let pi_path = temp.path().join(PROJECT_INSTRUCTIONS_FILENAME);
        fs::write(&pi_path, "test").unwrap();

        // Use chmod to make file unreadable
        Command::new("chmod")
            .args(["000", pi_path.to_str().unwrap()])
            .output()
            .unwrap();

        let result = load_project_instructions(temp.path()).unwrap();
        match result {
            ProjectInstructionResult::ReadError(msg) => {
                assert!(msg.contains("could not be read"));
            }
            _ => panic!("expected ReadError, got {:?}", result),
        }

        // Restore permissions
        let _ = Command::new("chmod")
            .args(["644", pi_path.to_str().unwrap()])
            .output();
    }

    #[test]
    fn test_get_project_instructions_or_error_missing() {
        let temp = TempDir::new().unwrap();
        let result = get_project_instructions_or_error(temp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_project_instructions_or_error_loaded() {
        let temp = TempDir::new().unwrap();
        let pi_content = "# Instructions";
        fs::write(temp.path().join(PROJECT_INSTRUCTIONS_FILENAME), pi_content).unwrap();

        let result = get_project_instructions_or_error(temp.path()).unwrap();
        assert_eq!(result.unwrap(), pi_content);
    }
}

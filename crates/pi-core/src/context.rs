use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub workspace_root: PathBuf,
}

impl ExecutionContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    pub fn resolve_path(&self, relative_path: &str) -> Result<PathBuf, super::Error> {
        let relative_path = Path::new(relative_path);

        if relative_path.is_absolute() {
            return Err(super::Error::InvalidInput(
                "absolute paths not allowed".into(),
            ));
        }

        let path_str = relative_path.to_string_lossy();
        if path_str.contains("..") {
            return Err(super::Error::InvalidInput(
                "path traversal not allowed".into(),
            ));
        }

        let target = self.workspace_root.join(relative_path);

        let canonical_workspace = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        if let Ok(canonical_target) = target.canonicalize() {
            if !canonical_target.starts_with(&canonical_workspace) {
                return Err(super::Error::InvalidInput("path escapes workspace".into()));
            }
        } else if !target.starts_with(&canonical_workspace) {
            return Err(super::Error::InvalidInput("path escapes workspace".into()));
        }

        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_context() -> (ExecutionContext, TempDir) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        (ExecutionContext::new(root), temp)
    }

    #[test]
    fn test_resolve_simple_relative_path() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("src/main.rs").unwrap();
        assert!(path.to_string_lossy().ends_with("src/main.rs"));
    }

    #[test]
    fn test_resolve_nested_path() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("a/b/c.txt").unwrap();
        assert!(path.to_string_lossy().ends_with("a/b/c.txt"));
    }

    #[test]
    fn test_reject_absolute_path() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("/etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_parent_traversal() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_nested_parent_traversal() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("a/../../b");
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_double_dot_in_path() {
        let (ctx, _temp) = create_context();
        let result = ctx.resolve_path("a/b/../c");
        assert!(result.is_err());
    }

    #[test]
    fn test_write_inside_workspace() {
        let (ctx, _temp) = create_context();
        let path = ctx.resolve_path("test.txt").unwrap();
        fs::write(&path, "hello").unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(path).unwrap();
        assert_eq!(content, "hello");
    }
}

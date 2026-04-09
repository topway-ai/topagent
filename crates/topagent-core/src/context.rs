use crate::approval::ApprovalMailbox;
use crate::checkpoint::WorkspaceCheckpointStore;
use crate::secrets::SecretRegistry;
use crate::{cancel::CancellationToken, runtime::RuntimeOptions};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub workspace_root: PathBuf,
    cancel_token: Option<CancellationToken>,
    secrets: SecretRegistry,
    memory_context: Option<String>,
    approval_mailbox: Option<ApprovalMailbox>,
    checkpoint_store: Option<WorkspaceCheckpointStore>,
}

impl ExecutionContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            cancel_token: None,
            secrets: SecretRegistry::new(),
            memory_context: None,
            approval_mailbox: None,
            checkpoint_store: None,
        }
    }

    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        self.cancel_token = Some(cancel_token);
        self
    }

    pub fn with_secrets(mut self, secrets: SecretRegistry) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_memory_context(mut self, memory_context: impl Into<String>) -> Self {
        let memory_context = memory_context.into();
        self.memory_context = if memory_context.trim().is_empty() {
            None
        } else {
            Some(memory_context)
        };
        self
    }

    pub fn with_approval_mailbox(mut self, approval_mailbox: ApprovalMailbox) -> Self {
        self.approval_mailbox = Some(approval_mailbox);
        self
    }

    pub fn with_workspace_checkpoint_store(
        mut self,
        checkpoint_store: WorkspaceCheckpointStore,
    ) -> Self {
        self.checkpoint_store = Some(checkpoint_store);
        self
    }

    pub fn secrets(&self) -> &SecretRegistry {
        &self.secrets
    }

    pub fn memory_context(&self) -> Option<&str> {
        self.memory_context.as_deref()
    }

    pub fn approval_mailbox(&self) -> Option<&ApprovalMailbox> {
        self.approval_mailbox.as_ref()
    }

    pub fn checkpoint_store(&self) -> Option<&WorkspaceCheckpointStore> {
        self.checkpoint_store.as_ref()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    pub fn cancel_token(&self) -> Option<&CancellationToken> {
        self.cancel_token.as_ref()
    }

    pub fn resolve_path(&self, relative_path: &str) -> Result<PathBuf, super::Error> {
        let relative_path = Path::new(relative_path);

        if relative_path.is_absolute() {
            return Err(super::Error::InvalidInput(
                "absolute paths not allowed".into(),
            ));
        }

        for component in relative_path.components() {
            match component {
                Component::ParentDir => {
                    return Err(super::Error::InvalidInput(
                        "path traversal not allowed".into(),
                    ));
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(super::Error::InvalidInput(
                        "path contains root or prefix component".into(),
                    ));
                }
                Component::Normal(_) | Component::CurDir => {}
            }
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

#[derive(Debug, Clone)]
pub struct ToolContext<'a> {
    pub exec: &'a ExecutionContext,
    pub runtime: &'a RuntimeOptions,
}

impl<'a> ToolContext<'a> {
    pub fn new(exec: &'a ExecutionContext, runtime: &'a RuntimeOptions) -> Self {
        Self { exec, runtime }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::WorkspaceCheckpointStore;
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

    #[test]
    fn test_memory_context_round_trip() {
        let (ctx, _temp) = create_context();
        let ctx = ctx.with_memory_context("memory");
        assert_eq!(ctx.memory_context(), Some("memory"));
    }

    #[test]
    fn test_checkpoint_store_round_trip() {
        let (ctx, temp) = create_context();
        let checkpoint = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        let ctx = ctx.with_workspace_checkpoint_store(checkpoint);
        assert_eq!(
            ctx.checkpoint_store().unwrap().latest_status().unwrap(),
            None
        );
    }
}

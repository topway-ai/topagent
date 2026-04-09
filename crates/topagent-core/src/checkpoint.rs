use crate::file_util::atomic_write;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CHECKPOINT_VERSION: u32 = 1;
const MAX_STORED_CHECKPOINTS: usize = 3;

#[derive(Debug, Clone)]
pub struct WorkspaceCheckpointStore {
    workspace_root: PathBuf,
    session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCheckpointStatus {
    pub id: String,
    pub created_at_unix_millis: u128,
    pub captured_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCheckpointRestoreReport {
    pub checkpoint_id: String,
    pub restored_files: Vec<String>,
    pub removed_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCheckpointManifest {
    version: u32,
    session_id: String,
    created_at_unix_millis: u128,
    entries: Vec<PersistedCheckpointEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCheckpointEntry {
    path: String,
    state: PersistedCheckpointEntryState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PersistedCheckpointEntryState {
    Existing { snapshot_rel_path: String },
    Missing,
}

impl WorkspaceCheckpointStore {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            session_id: format!("chk-{}-{}", std::process::id(), unix_timestamp_millis()),
        }
    }

    pub fn capture_file(&self, relative_path: &str) -> Result<bool> {
        if relative_path.trim().is_empty() {
            return Err(Error::InvalidInput(
                "checkpoint capture requires a relative file path".into(),
            ));
        }

        let mut manifest = self.load_or_create_session_manifest()?;
        if manifest
            .entries
            .iter()
            .any(|entry| entry.path == relative_path)
        {
            return Ok(false);
        }

        let full_path = self.workspace_root.join(relative_path);
        let state = if full_path.exists() {
            let snapshot_rel_path = format!(
                "files/{}-{}",
                manifest.entries.len(),
                hashed_snapshot_name(relative_path)
            );
            let snapshot_path = self
                .checkpoint_root_for(&manifest.session_id)
                .join(&snapshot_rel_path);
            if let Some(parent) = snapshot_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::ToolFailed(format!(
                        "failed to create checkpoint directory {}: {}",
                        parent.display(),
                        err
                    ))
                })?;
            }
            fs::copy(&full_path, &snapshot_path).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to snapshot {} to {}: {}",
                    full_path.display(),
                    snapshot_path.display(),
                    err
                ))
            })?;
            PersistedCheckpointEntryState::Existing { snapshot_rel_path }
        } else {
            PersistedCheckpointEntryState::Missing
        };

        manifest.entries.push(PersistedCheckpointEntry {
            path: relative_path.to_string(),
            state,
        });
        self.save_manifest(&manifest)?;
        self.prune_old_checkpoints()?;
        Ok(true)
    }

    pub fn latest_status(&self) -> Result<Option<WorkspaceCheckpointStatus>> {
        let Some((_, manifest)) = self.load_latest_manifest()? else {
            return Ok(None);
        };

        Ok(Some(WorkspaceCheckpointStatus {
            id: manifest.session_id,
            created_at_unix_millis: manifest.created_at_unix_millis,
            captured_paths: manifest
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect(),
        }))
    }

    pub fn latest_diff_preview(&self) -> Result<Option<String>> {
        let Some((_, manifest)) = self.load_latest_manifest()? else {
            return Ok(None);
        };

        let mut diffs = Vec::new();
        for entry in &manifest.entries {
            if let Some(diff) = self.diff_entry(&manifest, entry)? {
                diffs.push(diff);
            }
        }

        if diffs.is_empty() {
            return Ok(Some(
                "Workspace already matches the latest checkpoint.".to_string(),
            ));
        }

        Ok(Some(diffs.join("\n")))
    }

    pub fn restore_latest(&self) -> Result<Option<WorkspaceCheckpointRestoreReport>> {
        let Some((manifest_path, manifest)) = self.load_latest_manifest()? else {
            return Ok(None);
        };

        let mut restored_files = Vec::new();
        let mut removed_files = Vec::new();
        for entry in &manifest.entries {
            let workspace_path = self.workspace_root.join(&entry.path);
            match &entry.state {
                PersistedCheckpointEntryState::Existing { snapshot_rel_path } => {
                    let snapshot_path = self
                        .checkpoint_root_for(&manifest.session_id)
                        .join(snapshot_rel_path);
                    if let Some(parent) = workspace_path.parent() {
                        fs::create_dir_all(parent).map_err(|err| {
                            Error::ToolFailed(format!(
                                "failed to create parent directory {} during restore: {}",
                                parent.display(),
                                err
                            ))
                        })?;
                    }
                    fs::copy(&snapshot_path, &workspace_path).map_err(|err| {
                        Error::ToolFailed(format!(
                            "failed to restore {} from {}: {}",
                            workspace_path.display(),
                            snapshot_path.display(),
                            err
                        ))
                    })?;
                    restored_files.push(entry.path.clone());
                }
                PersistedCheckpointEntryState::Missing => {
                    if workspace_path.exists() {
                        remove_path_if_exists(&workspace_path)?;
                        removed_files.push(entry.path.clone());
                    }
                }
            }
        }

        let checkpoint_root = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                Error::ToolFailed(format!(
                    "invalid checkpoint manifest path {}",
                    manifest_path.display()
                ))
            })?;
        fs::remove_dir_all(&checkpoint_root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to remove checkpoint directory {} after restore: {}",
                checkpoint_root.display(),
                err
            ))
        })?;

        Ok(Some(WorkspaceCheckpointRestoreReport {
            checkpoint_id: manifest.session_id,
            restored_files,
            removed_files,
        }))
    }

    fn load_or_create_session_manifest(&self) -> Result<PersistedCheckpointManifest> {
        let manifest_path = self.manifest_path_for(&self.session_id);
        if manifest_path.exists() {
            return Self::load_manifest_path(&manifest_path);
        }

        let checkpoint_root = self.checkpoint_root_for(&self.session_id);
        fs::create_dir_all(&checkpoint_root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to create checkpoint directory {}: {}",
                checkpoint_root.display(),
                err
            ))
        })?;

        let manifest = PersistedCheckpointManifest {
            version: CHECKPOINT_VERSION,
            session_id: self.session_id.clone(),
            created_at_unix_millis: unix_timestamp_millis(),
            entries: Vec::new(),
        };
        self.save_manifest(&manifest)?;
        Ok(manifest)
    }

    fn load_latest_manifest(&self) -> Result<Option<(PathBuf, PersistedCheckpointManifest)>> {
        let root = self.checkpoints_root();
        if !root.exists() {
            return Ok(None);
        }

        let mut manifests = Vec::new();
        for entry in fs::read_dir(&root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read checkpoint directory {}: {}",
                root.display(),
                err
            ))
        })? {
            let entry = entry.map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read checkpoint entry in {}: {}",
                    root.display(),
                    err
                ))
            })?;
            let checkpoint_root = entry.path();
            if !checkpoint_root.is_dir() {
                continue;
            }
            let manifest_path = checkpoint_root.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = Self::load_manifest_path(&manifest_path)?;
            manifests.push((manifest_path, manifest));
        }

        manifests.sort_by(|a, b| {
            a.1.created_at_unix_millis
                .cmp(&b.1.created_at_unix_millis)
                .then(a.1.session_id.cmp(&b.1.session_id))
        });
        Ok(manifests.pop())
    }

    fn save_manifest(&self, manifest: &PersistedCheckpointManifest) -> Result<()> {
        let manifest_path = self.manifest_path_for(&manifest.session_id);
        if let Some(parent) = manifest_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to create checkpoint directory {}: {}",
                    parent.display(),
                    err
                ))
            })?;
        }
        let contents = serde_json::to_string_pretty(manifest).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to serialize checkpoint manifest {}: {}",
                manifest_path.display(),
                err
            ))
        })?;
        atomic_write(&manifest_path, &contents)?;
        Ok(())
    }

    fn prune_old_checkpoints(&self) -> Result<()> {
        let root = self.checkpoints_root();
        if !root.exists() {
            return Ok(());
        }

        let mut manifests = Vec::new();
        for entry in fs::read_dir(&root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read checkpoint directory {}: {}",
                root.display(),
                err
            ))
        })? {
            let entry = entry.map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read checkpoint entry in {}: {}",
                    root.display(),
                    err
                ))
            })?;
            let checkpoint_root = entry.path();
            if !checkpoint_root.is_dir() {
                continue;
            }
            let manifest_path = checkpoint_root.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = Self::load_manifest_path(&manifest_path)?;
            manifests.push((checkpoint_root, manifest));
        }

        manifests.sort_by(|a, b| {
            b.1.created_at_unix_millis
                .cmp(&a.1.created_at_unix_millis)
                .then(b.1.session_id.cmp(&a.1.session_id))
        });

        for (checkpoint_root, _) in manifests.into_iter().skip(MAX_STORED_CHECKPOINTS) {
            fs::remove_dir_all(&checkpoint_root).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to prune checkpoint directory {}: {}",
                    checkpoint_root.display(),
                    err
                ))
            })?;
        }

        Ok(())
    }

    fn diff_entry(
        &self,
        manifest: &PersistedCheckpointManifest,
        entry: &PersistedCheckpointEntry,
    ) -> Result<Option<String>> {
        let current_path = self.workspace_root.join(&entry.path);
        let snapshot_path = match &entry.state {
            PersistedCheckpointEntryState::Existing { snapshot_rel_path } => Some(
                self.checkpoint_root_for(&manifest.session_id)
                    .join(snapshot_rel_path),
            ),
            PersistedCheckpointEntryState::Missing => None,
        };

        if paths_match(
            snapshot_path.as_deref(),
            current_path.exists().then_some(current_path.as_path()),
        )? {
            return Ok(None);
        }

        let left_path = snapshot_path
            .as_deref()
            .unwrap_or_else(|| Path::new(null_device_path()));
        let right_path = if current_path.exists() {
            current_path.as_path()
        } else {
            Path::new(null_device_path())
        };

        let output = Command::new("git")
            .arg("diff")
            .arg("--no-index")
            .arg("--no-ext-diff")
            .arg("--src-prefix=checkpoint/")
            .arg("--dst-prefix=workspace/")
            .arg("--")
            .arg(left_path)
            .arg(right_path)
            .output()
            .map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to build checkpoint diff preview for {}: {}",
                    entry.path, err
                ))
            })?;

        let exit_code = output.status.code().unwrap_or(-1);
        if exit_code != 0 && exit_code != 1 {
            return Err(Error::ToolFailed(format!(
                "checkpoint diff preview failed for {}: {}",
                entry.path,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    fn checkpoints_root(&self) -> PathBuf {
        self.workspace_root.join(".topagent").join("checkpoints")
    }

    fn checkpoint_root_for(&self, session_id: &str) -> PathBuf {
        self.checkpoints_root().join(session_id)
    }

    fn manifest_path_for(&self, session_id: &str) -> PathBuf {
        self.checkpoint_root_for(session_id).join("manifest.json")
    }

    fn load_manifest_path(path: &Path) -> Result<PersistedCheckpointManifest> {
        let contents = fs::read_to_string(path).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read checkpoint manifest {}: {}",
                path.display(),
                err
            ))
        })?;
        let manifest: PersistedCheckpointManifest =
            serde_json::from_str(&contents).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to parse checkpoint manifest {}: {}",
                    path.display(),
                    err
                ))
            })?;
        if manifest.version != CHECKPOINT_VERSION {
            return Err(Error::ToolFailed(format!(
                "unsupported checkpoint manifest version {} in {}",
                manifest.version,
                path.display()
            )));
        }
        Ok(manifest)
    }
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn hashed_snapshot_name(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    format!("{:x}.snapshot", hasher.finalize())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|err| {
            Error::ToolFailed(format!("failed to remove {}: {}", path.display(), err))
        })?;
    } else {
        fs::remove_file(path).map_err(|err| {
            Error::ToolFailed(format!("failed to remove {}: {}", path.display(), err))
        })?;
    }
    Ok(())
}

fn paths_match(original: Option<&Path>, current: Option<&Path>) -> Result<bool> {
    match (original, current) {
        (None, None) => Ok(true),
        (Some(_), None) | (None, Some(_)) => Ok(false),
        (Some(left), Some(right)) => {
            let left_bytes = fs::read(left).map_err(|err| {
                Error::ToolFailed(format!("failed to read {}: {}", left.display(), err))
            })?;
            let right_bytes = fs::read(right).map_err(|err| {
                Error::ToolFailed(format!("failed to read {}: {}", right.display(), err))
            })?;
            Ok(left_bytes == right_bytes)
        }
    }
}

fn null_device_path() -> &'static str {
    #[cfg(windows)]
    {
        "NUL"
    }
    #[cfg(not(windows))]
    {
        "/dev/null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_capture_and_restore_existing_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("notes.txt");
        fs::write(&file_path, "before").unwrap();

        let store = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        store.capture_file("notes.txt").unwrap();
        fs::write(&file_path, "after").unwrap();

        let status = store.latest_status().unwrap().unwrap();
        assert_eq!(status.captured_paths, vec!["notes.txt"]);

        let diff = store.latest_diff_preview().unwrap().unwrap();
        assert!(diff.contains("workspace/"));
        assert!(diff.contains("after"));

        let report = store.restore_latest().unwrap().unwrap();
        assert_eq!(report.restored_files, vec!["notes.txt"]);
        assert!(report.removed_files.is_empty());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "before");
        assert!(store.latest_status().unwrap().is_none());
    }

    #[test]
    fn test_capture_new_file_and_restore_removes_it() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("new.txt");

        let store = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        store.capture_file("new.txt").unwrap();
        fs::write(&file_path, "created later").unwrap();

        let diff = store.latest_diff_preview().unwrap().unwrap();
        assert!(diff.contains("workspace/"));

        let report = store.restore_latest().unwrap().unwrap();
        assert!(report.restored_files.is_empty());
        assert_eq!(report.removed_files, vec!["new.txt"]);
        assert!(!file_path.exists());
    }

    #[test]
    fn test_latest_checkpoint_prefers_newest_capture() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("first.txt"), "first").unwrap();
        let first = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        first.capture_file("first.txt").unwrap();

        thread::sleep(Duration::from_millis(2));

        fs::write(temp.path().join("second.txt"), "second").unwrap();
        let second = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
        second.capture_file("second.txt").unwrap();

        let status = second.latest_status().unwrap().unwrap();
        assert_eq!(status.captured_paths, vec!["second.txt"]);
    }

    #[test]
    fn test_capture_prunes_old_checkpoints() {
        let temp = TempDir::new().unwrap();

        for index in 0..5 {
            let file = format!("file-{index}.txt");
            fs::write(temp.path().join(&file), format!("value-{index}")).unwrap();
            let store = WorkspaceCheckpointStore::new(temp.path().to_path_buf());
            store.capture_file(&file).unwrap();
            thread::sleep(Duration::from_millis(2));
        }

        let checkpoint_root = temp.path().join(".topagent/checkpoints");
        let dirs = fs::read_dir(&checkpoint_root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .count();
        assert_eq!(dirs, MAX_STORED_CHECKPOINTS);
    }
}

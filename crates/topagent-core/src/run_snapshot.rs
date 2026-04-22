use crate::file_util::atomic_write;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const RUN_SNAPSHOT_VERSION: u32 = 1;
const MAX_STORED_RUN_SNAPSHOTS: usize = 3;
const WORKSPACE_ROOT_PATH: &str = ".";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSnapshotCaptureSource {
    Write,
    Edit,
    Bash,
}

impl RunSnapshotCaptureSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Bash => "bash",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSnapshotCaptureMetadata {
    pub source: RunSnapshotCaptureSource,
    pub reason: String,
    pub detail: Option<String>,
}

impl RunSnapshotCaptureMetadata {
    pub fn new(source: RunSnapshotCaptureSource, reason: impl Into<String>) -> Self {
        Self {
            source,
            reason: reason.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = if detail.trim().is_empty() {
            None
        } else {
            Some(detail)
        };
        self
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceRunSnapshotStore {
    workspace_root: PathBuf,
    session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRunSnapshotCaptureStatus {
    pub source: RunSnapshotCaptureSource,
    pub reason: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRunSnapshotStatus {
    pub id: String,
    pub created_at_unix_millis: u128,
    pub captures: Vec<WorkspaceRunSnapshotCaptureStatus>,
    pub captured_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRunSnapshotRestoreReport {
    pub snapshot_id: String,
    pub restored_files: Vec<String>,
    pub removed_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRunSnapshotManifest {
    version: u32,
    session_id: String,
    created_at_unix_millis: u128,
    #[serde(default)]
    captures: Vec<PersistedRunSnapshotCapture>,
    entries: Vec<PersistedRunSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRunSnapshotCapture {
    source: RunSnapshotCaptureSource,
    reason: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRunSnapshotEntry {
    path: String,
    state: PersistedRunSnapshotEntryState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PersistedRunSnapshotEntryState {
    Existing { snapshot_rel_path: String },
    ExistingDirectory,
    Missing,
}

impl WorkspaceRunSnapshotStore {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            session_id: format!(
                "run-snap-{}-{}",
                std::process::id(),
                unix_timestamp_millis()
            ),
        }
    }

    pub fn capture_file(
        &self,
        relative_path: &str,
        metadata: RunSnapshotCaptureMetadata,
    ) -> Result<usize> {
        self.capture_paths(&[relative_path.to_string()], metadata)
    }

    pub fn capture_paths(
        &self,
        relative_paths: &[String],
        metadata: RunSnapshotCaptureMetadata,
    ) -> Result<usize> {
        if relative_paths.is_empty() {
            return Ok(0);
        }

        let mut manifest = self.load_or_create_session_manifest()?;
        manifest
            .captures
            .push(PersistedRunSnapshotCapture::from(metadata));

        let mut captured = 0usize;
        let mut seen = HashSet::new();
        for relative_path in relative_paths {
            let normalized = normalize_relative_path(relative_path)?;
            if !seen.insert(normalized.clone()) {
                continue;
            }
            captured += self.capture_relative_path(&mut manifest, Path::new(&normalized))?;
        }

        self.save_manifest(&manifest)?;
        self.prune_old_run_snapshots()?;
        Ok(captured)
    }

    pub fn capture_workspace(&self, metadata: RunSnapshotCaptureMetadata) -> Result<usize> {
        let mut manifest = self.load_or_create_session_manifest()?;
        manifest
            .captures
            .push(PersistedRunSnapshotCapture::from(metadata));
        let captured = self.capture_relative_path(&mut manifest, Path::new(WORKSPACE_ROOT_PATH))?;
        self.save_manifest(&manifest)?;
        self.prune_old_run_snapshots()?;
        Ok(captured)
    }

    pub fn latest_status(&self) -> Result<Option<WorkspaceRunSnapshotStatus>> {
        let Some((_, manifest)) = self.load_latest_manifest()? else {
            return Ok(None);
        };

        Ok(Some(WorkspaceRunSnapshotStatus {
            id: manifest.session_id,
            created_at_unix_millis: manifest.created_at_unix_millis,
            captures: manifest
                .captures
                .into_iter()
                .map(WorkspaceRunSnapshotCaptureStatus::from)
                .collect(),
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
                "Workspace already matches the latest run snapshot.".to_string(),
            ));
        }

        Ok(Some(diffs.join("\n")))
    }

    pub fn restore_latest(&self) -> Result<Option<WorkspaceRunSnapshotRestoreReport>> {
        let Some((manifest_path, manifest)) = self.load_latest_manifest()? else {
            return Ok(None);
        };

        let snapshot_root = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                Error::ToolFailed(format!(
                    "invalid run snapshot manifest path {}",
                    manifest_path.display()
                ))
            })?;

        let snapshot_paths = manifest
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();

        let mut existing_dirs = manifest
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.state,
                    PersistedRunSnapshotEntryState::ExistingDirectory
                )
            })
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        existing_dirs.sort_by_key(|path| path_depth(path));

        let mut removed_files = Vec::new();
        for relative_dir in existing_dirs {
            removed_files.extend(self.remove_unexpected_children(
                &relative_dir,
                &snapshot_paths,
                &snapshot_root,
            )?);
        }

        let mut restored_files = Vec::new();
        for entry in &manifest.entries {
            let workspace_path = self.workspace_path_for(&entry.path);
            match &entry.state {
                PersistedRunSnapshotEntryState::Existing { snapshot_rel_path } => {
                    if workspace_path.is_dir() {
                        remove_path_if_exists(&workspace_path)?;
                    }
                    if let Some(parent) = workspace_path.parent() {
                        fs::create_dir_all(parent).map_err(|err| {
                            Error::ToolFailed(format!(
                                "failed to create parent directory {} during restore: {}",
                                parent.display(),
                                err
                            ))
                        })?;
                    }
                    let snapshot_path = snapshot_root.join(snapshot_rel_path);
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
                PersistedRunSnapshotEntryState::ExistingDirectory => {
                    if workspace_path.is_file() {
                        remove_path_if_exists(&workspace_path)?;
                    }
                    if entry.path != WORKSPACE_ROOT_PATH {
                        fs::create_dir_all(&workspace_path).map_err(|err| {
                            Error::ToolFailed(format!(
                                "failed to recreate directory {} during restore: {}",
                                workspace_path.display(),
                                err
                            ))
                        })?;
                    }
                }
                PersistedRunSnapshotEntryState::Missing => {
                    if workspace_path.exists() {
                        remove_path_if_exists(&workspace_path)?;
                        removed_files.push(entry.path.clone());
                    }
                }
            }
        }

        fs::remove_dir_all(&snapshot_root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to remove run snapshot directory {} after restore: {}",
                snapshot_root.display(),
                err
            ))
        })?;

        restored_files.sort();
        restored_files.dedup();
        removed_files.sort();
        removed_files.dedup();

        Ok(Some(WorkspaceRunSnapshotRestoreReport {
            snapshot_id: manifest.session_id,
            restored_files,
            removed_files,
        }))
    }

    fn capture_relative_path(
        &self,
        manifest: &mut PersistedRunSnapshotManifest,
        relative_path: &Path,
    ) -> Result<usize> {
        let normalized = normalize_relative_path_path(relative_path)?;
        if should_skip_internal_path(&normalized) {
            return Ok(0);
        }
        if manifest
            .entries
            .iter()
            .any(|entry| entry.path == normalized)
        {
            return Ok(0);
        }

        let full_path = self.workspace_path_for(&normalized);
        if full_path.is_dir() {
            manifest.entries.push(PersistedRunSnapshotEntry {
                path: normalized.clone(),
                state: PersistedRunSnapshotEntryState::ExistingDirectory,
            });

            let mut captured = 1usize;
            for child in fs::read_dir(&full_path).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read directory {} for run snapshot capture: {}",
                    full_path.display(),
                    err
                ))
            })? {
                let child = child.map_err(|err| {
                    Error::ToolFailed(format!(
                        "failed to read directory entry in {}: {}",
                        full_path.display(),
                        err
                    ))
                })?;
                let child_name = child.file_name();
                let child_relative = if normalized == WORKSPACE_ROOT_PATH {
                    PathBuf::from(child_name)
                } else {
                    PathBuf::from(&normalized).join(child_name)
                };
                captured += self.capture_relative_path(manifest, &child_relative)?;
            }
            return Ok(captured);
        }

        let state = if full_path.is_file() {
            let snapshot_rel_path = format!(
                "files/{}-{}",
                manifest.entries.len(),
                hashed_snapshot_name(&normalized)
            );
            let snapshot_path = self
                .run_snapshot_root_for(&manifest.session_id)
                .join(&snapshot_rel_path);
            if let Some(parent) = snapshot_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::ToolFailed(format!(
                        "failed to create run snapshot directory {}: {}",
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
            PersistedRunSnapshotEntryState::Existing { snapshot_rel_path }
        } else {
            PersistedRunSnapshotEntryState::Missing
        };

        manifest.entries.push(PersistedRunSnapshotEntry {
            path: normalized,
            state,
        });
        Ok(1)
    }

    fn load_or_create_session_manifest(&self) -> Result<PersistedRunSnapshotManifest> {
        let manifest_path = self.manifest_path_for(&self.session_id);
        if manifest_path.exists() {
            return Self::load_manifest_path(&manifest_path);
        }

        let snapshot_root = self.run_snapshot_root_for(&self.session_id);
        fs::create_dir_all(&snapshot_root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to create run snapshot directory {}: {}",
                snapshot_root.display(),
                err
            ))
        })?;

        let manifest = PersistedRunSnapshotManifest {
            version: RUN_SNAPSHOT_VERSION,
            session_id: self.session_id.clone(),
            created_at_unix_millis: unix_timestamp_millis(),
            captures: Vec::new(),
            entries: Vec::new(),
        };
        self.save_manifest(&manifest)?;
        Ok(manifest)
    }

    fn load_latest_manifest(&self) -> Result<Option<(PathBuf, PersistedRunSnapshotManifest)>> {
        let root = self.run_snapshots_root();
        if !root.exists() {
            return Ok(None);
        }

        let mut manifests = Vec::new();
        for entry in fs::read_dir(&root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read run snapshot directory {}: {}",
                root.display(),
                err
            ))
        })? {
            let entry = entry.map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read run snapshot entry in {}: {}",
                    root.display(),
                    err
                ))
            })?;
            let snapshot_root = entry.path();
            if !snapshot_root.is_dir() {
                continue;
            }
            let manifest_path = snapshot_root.join("manifest.json");
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

    fn save_manifest(&self, manifest: &PersistedRunSnapshotManifest) -> Result<()> {
        let manifest_path = self.manifest_path_for(&manifest.session_id);
        if let Some(parent) = manifest_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to create run snapshot directory {}: {}",
                    parent.display(),
                    err
                ))
            })?;
        }
        let contents = serde_json::to_string_pretty(manifest).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to serialize run snapshot manifest {}: {}",
                manifest_path.display(),
                err
            ))
        })?;
        atomic_write(&manifest_path, &contents)?;
        Ok(())
    }

    fn prune_old_run_snapshots(&self) -> Result<()> {
        let root = self.run_snapshots_root();
        if !root.exists() {
            return Ok(());
        }

        let mut manifests = Vec::new();
        for entry in fs::read_dir(&root).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read run snapshot directory {}: {}",
                root.display(),
                err
            ))
        })? {
            let entry = entry.map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to read run snapshot entry in {}: {}",
                    root.display(),
                    err
                ))
            })?;
            let snapshot_root = entry.path();
            if !snapshot_root.is_dir() {
                continue;
            }
            let manifest_path = snapshot_root.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = Self::load_manifest_path(&manifest_path)?;
            manifests.push((snapshot_root, manifest));
        }

        manifests.sort_by(|a, b| {
            b.1.created_at_unix_millis
                .cmp(&a.1.created_at_unix_millis)
                .then(b.1.session_id.cmp(&a.1.session_id))
        });

        for (snapshot_root, _) in manifests.into_iter().skip(MAX_STORED_RUN_SNAPSHOTS) {
            fs::remove_dir_all(&snapshot_root).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to prune run snapshot directory {}: {}",
                    snapshot_root.display(),
                    err
                ))
            })?;
        }

        Ok(())
    }

    fn remove_unexpected_children(
        &self,
        relative_dir: &str,
        snapshot_paths: &HashSet<String>,
        snapshot_root: &Path,
    ) -> Result<Vec<String>> {
        let dir_path = self.workspace_path_for(relative_dir);
        if !dir_path.is_dir() {
            return Ok(Vec::new());
        }

        let descendants = collect_descendant_paths(&dir_path, relative_dir)?;
        let mut removed = Vec::new();
        for relative_path in descendants
            .into_iter()
            .filter(|relative_path| !should_skip_internal_path(relative_path))
            .filter(|relative_path| !is_active_run_snapshot_path(relative_path, snapshot_root))
        {
            if snapshot_paths.contains(&relative_path)
                || snapshot_has_descendant(snapshot_paths, &relative_path)
            {
                continue;
            }

            let full_path = self.workspace_path_for(&relative_path);
            if full_path.exists() {
                remove_path_if_exists(&full_path)?;
                removed.push(relative_path);
            }
        }

        Ok(removed)
    }

    fn diff_entry(
        &self,
        manifest: &PersistedRunSnapshotManifest,
        entry: &PersistedRunSnapshotEntry,
    ) -> Result<Option<String>> {
        let current_path = self.workspace_path_for(&entry.path);
        match &entry.state {
            PersistedRunSnapshotEntryState::Existing { snapshot_rel_path } => {
                if current_path.is_dir() {
                    return Ok(Some(format!(
                        "Directory replaced snapshotted file: {}\n",
                        entry.path
                    )));
                }

                let snapshot_path = self
                    .run_snapshot_root_for(&manifest.session_id)
                    .join(snapshot_rel_path);
                if paths_match(
                    Some(snapshot_path.as_path()),
                    current_path.exists().then_some(current_path.as_path()),
                )? {
                    return Ok(None);
                }

                let right_path = if current_path.exists() {
                    current_path.as_path()
                } else {
                    Path::new(null_device_path())
                };
                diff_files(&entry.path, snapshot_path.as_path(), right_path)
            }
            PersistedRunSnapshotEntryState::ExistingDirectory => {
                if current_path.is_dir() {
                    Ok(None)
                } else if current_path.exists() {
                    Ok(Some(format!(
                        "File replaced snapshotted directory: {}\n",
                        entry.path
                    )))
                } else {
                    Ok(Some(format!(
                        "Directory missing from workspace: {}\n",
                        entry.path
                    )))
                }
            }
            PersistedRunSnapshotEntryState::Missing => {
                if !current_path.exists() {
                    return Ok(None);
                }

                if current_path.is_dir() {
                    return Ok(Some(format!(
                        "Directory added after run snapshot: {}\n",
                        entry.path
                    )));
                }

                diff_files(
                    &entry.path,
                    Path::new(null_device_path()),
                    current_path.as_path(),
                )
            }
        }
    }

    fn run_snapshots_root(&self) -> PathBuf {
        self.workspace_root.join(".topagent").join("run-snapshots")
    }

    fn run_snapshot_root_for(&self, session_id: &str) -> PathBuf {
        self.run_snapshots_root().join(session_id)
    }

    fn manifest_path_for(&self, session_id: &str) -> PathBuf {
        self.run_snapshot_root_for(session_id).join("manifest.json")
    }

    fn workspace_path_for(&self, relative_path: &str) -> PathBuf {
        if relative_path == WORKSPACE_ROOT_PATH {
            self.workspace_root.clone()
        } else {
            self.workspace_root.join(relative_path)
        }
    }

    fn load_manifest_path(path: &Path) -> Result<PersistedRunSnapshotManifest> {
        let contents = fs::read_to_string(path).map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read run snapshot manifest {}: {}",
                path.display(),
                err
            ))
        })?;
        let manifest: PersistedRunSnapshotManifest =
            serde_json::from_str(&contents).map_err(|err| {
                Error::ToolFailed(format!(
                    "failed to parse run snapshot manifest {}: {}",
                    path.display(),
                    err
                ))
            })?;
        if manifest.version != RUN_SNAPSHOT_VERSION {
            return Err(Error::ToolFailed(format!(
                "unsupported run snapshot manifest version {} in {}",
                manifest.version,
                path.display()
            )));
        }
        Ok(manifest)
    }
}

impl From<RunSnapshotCaptureMetadata> for PersistedRunSnapshotCapture {
    fn from(metadata: RunSnapshotCaptureMetadata) -> Self {
        Self {
            source: metadata.source,
            reason: metadata.reason,
            detail: metadata.detail,
        }
    }
}

impl From<PersistedRunSnapshotCapture> for WorkspaceRunSnapshotCaptureStatus {
    fn from(capture: PersistedRunSnapshotCapture) -> Self {
        Self {
            source: capture.source,
            reason: capture.reason,
            detail: capture.detail,
        }
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

fn diff_files(path_label: &str, left_path: &Path, right_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--no-index")
        .arg("--no-ext-diff")
        .arg("--src-prefix=run-snapshot/")
        .arg("--dst-prefix=workspace/")
        .arg("--")
        .arg(left_path)
        .arg(right_path)
        .output()
        .map_err(|err| {
            Error::ToolFailed(format!(
                "failed to build run snapshot diff preview for {}: {}",
                path_label, err
            ))
        })?;

    let exit_code = output.status.code().unwrap_or(-1);
    if exit_code != 0 && exit_code != 1 {
        return Err(Error::ToolFailed(format!(
            "run snapshot diff preview failed for {}: {}",
            path_label,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

fn normalize_relative_path(path: &str) -> Result<String> {
    normalize_relative_path_path(Path::new(path))
}

fn normalize_relative_path_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(Error::InvalidInput(
            "absolute paths not allowed in run snapshot capture".into(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(Error::InvalidInput(
                    "path traversal not allowed in run snapshot capture".into(),
                ));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(Error::InvalidInput(
                    "root-prefixed paths not allowed in run snapshot capture".into(),
                ));
            }
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        return Ok(WORKSPACE_ROOT_PATH.to_string());
    }

    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

fn path_depth(path: &str) -> usize {
    if path == WORKSPACE_ROOT_PATH {
        0
    } else {
        path.matches('/').count() + 1
    }
}

fn snapshot_has_descendant(snapshot_paths: &HashSet<String>, path: &str) -> bool {
    let prefix = format!("{path}/");
    snapshot_paths
        .iter()
        .any(|entry| entry.starts_with(&prefix))
}

fn should_skip_internal_path(path: &str) -> bool {
    if path == WORKSPACE_ROOT_PATH {
        return false;
    }
    path == ".git"
        || path.starts_with(".git/")
        || path == ".topagent"
        || path.starts_with(".topagent/")
}

fn is_active_run_snapshot_path(relative_path: &str, snapshot_root: &Path) -> bool {
    let Some(run_snapshots_root) = snapshot_root.parent() else {
        return false;
    };
    snapshot_root
        .parent()
        .and_then(Path::parent)
        .map(|workspace_root| {
            workspace_root
                .join(relative_path)
                .starts_with(run_snapshots_root)
        })
        .unwrap_or(false)
}

fn collect_descendant_paths(dir_path: &Path, relative_dir: &str) -> Result<Vec<String>> {
    let mut collected = Vec::new();
    collect_descendant_paths_recursive(dir_path, relative_dir, &mut collected)?;
    collected.sort_by_key(|path| std::cmp::Reverse(path_depth(path)));
    Ok(collected)
}

fn collect_descendant_paths_recursive(
    dir_path: &Path,
    relative_dir: &str,
    collected: &mut Vec<String>,
) -> Result<()> {
    if !dir_path.is_dir() {
        return Ok(());
    }

    for child in fs::read_dir(dir_path).map_err(|err| {
        Error::ToolFailed(format!(
            "failed to read directory {} during run snapshot restore: {}",
            dir_path.display(),
            err
        ))
    })? {
        let child = child.map_err(|err| {
            Error::ToolFailed(format!(
                "failed to read directory entry in {} during run snapshot restore: {}",
                dir_path.display(),
                err
            ))
        })?;
        let child_name = child.file_name().to_string_lossy().to_string();
        let child_relative = if relative_dir == WORKSPACE_ROOT_PATH {
            child_name
        } else {
            format!("{relative_dir}/{child_name}")
        };
        collected.push(child_relative.clone());
        let child_path = child.path();
        if child_path.is_dir() {
            collect_descendant_paths_recursive(&child_path, &child_relative, collected)?;
        }
    }

    Ok(())
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

        let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        store
            .capture_file(
                "notes.txt",
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Write,
                    "structured write",
                ),
            )
            .unwrap();
        fs::write(&file_path, "after").unwrap();

        let status = store.latest_status().unwrap().unwrap();
        assert_eq!(status.captured_paths, vec!["notes.txt"]);
        assert_eq!(status.captures.len(), 1);
        assert_eq!(status.captures[0].source, RunSnapshotCaptureSource::Write);

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

        let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        store
            .capture_file(
                "new.txt",
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Write,
                    "structured write",
                ),
            )
            .unwrap();
        fs::write(&file_path, "created later").unwrap();

        let diff = store.latest_diff_preview().unwrap().unwrap();
        assert!(diff.contains("workspace/"));

        let report = store.restore_latest().unwrap().unwrap();
        assert!(report.restored_files.is_empty());
        assert_eq!(report.removed_files, vec!["new.txt"]);
        assert!(!file_path.exists());
    }

    #[test]
    fn test_directory_capture_restores_and_removes_unexpected_children() {
        let temp = TempDir::new().unwrap();
        let dir_path = temp.path().join("src");
        fs::create_dir_all(&dir_path).unwrap();
        fs::write(dir_path.join("lib.rs"), "before").unwrap();

        let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        store
            .capture_paths(
                &["src".to_string()],
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Bash,
                    "directory mutation",
                ),
            )
            .unwrap();

        fs::remove_file(dir_path.join("lib.rs")).unwrap();
        fs::write(dir_path.join("new.rs"), "new").unwrap();

        let report = store.restore_latest().unwrap().unwrap();
        assert!(report.restored_files.contains(&"src/lib.rs".to_string()));
        assert!(report.removed_files.contains(&"src/new.rs".to_string()));
        assert_eq!(
            fs::read_to_string(dir_path.join("lib.rs")).unwrap(),
            "before"
        );
        assert!(!dir_path.join("new.rs").exists());
    }

    #[test]
    fn test_workspace_capture_restores_and_removes_new_root_files() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "before").unwrap();

        let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        store
            .capture_workspace(
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Bash,
                    "workspace rewrite",
                )
                .with_detail("git reset --hard"),
            )
            .unwrap();

        fs::write(temp.path().join("src/lib.rs"), "after").unwrap();
        fs::write(temp.path().join("new.txt"), "created").unwrap();

        let report = store.restore_latest().unwrap().unwrap();
        assert!(report.restored_files.contains(&"src/lib.rs".to_string()));
        assert!(report.removed_files.contains(&"new.txt".to_string()));
        assert_eq!(
            fs::read_to_string(temp.path().join("src/lib.rs")).unwrap(),
            "before"
        );
        assert!(!temp.path().join("new.txt").exists());
    }

    #[test]
    fn test_latest_run_snapshot_prefers_newest_capture() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("first.txt"), "first").unwrap();
        let first = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        first
            .capture_file(
                "first.txt",
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Write,
                    "structured write",
                ),
            )
            .unwrap();

        thread::sleep(Duration::from_millis(2));

        fs::write(temp.path().join("second.txt"), "second").unwrap();
        let second = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
        second
            .capture_file(
                "second.txt",
                RunSnapshotCaptureMetadata::new(
                    RunSnapshotCaptureSource::Bash,
                    "shell redirection",
                )
                .with_detail("echo hi > second.txt"),
            )
            .unwrap();

        let status = second.latest_status().unwrap().unwrap();
        assert_eq!(status.captured_paths, vec!["second.txt"]);
        assert_eq!(status.captures[0].source, RunSnapshotCaptureSource::Bash);
        assert_eq!(status.captures[0].reason, "shell redirection");
    }

    #[test]
    fn test_capture_prunes_old_run_snapshots() {
        let temp = TempDir::new().unwrap();

        for index in 0..5 {
            let file = format!("file-{index}.txt");
            fs::write(temp.path().join(&file), format!("value-{index}")).unwrap();
            let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
            store
                .capture_file(
                    &file,
                    RunSnapshotCaptureMetadata::new(
                        RunSnapshotCaptureSource::Write,
                        "structured write",
                    ),
                )
                .unwrap();
            thread::sleep(Duration::from_millis(2));
        }

        let snapshot_root = temp.path().join(".topagent/run-snapshots");
        let dirs = fs::read_dir(&snapshot_root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .count();
        assert_eq!(dirs, MAX_STORED_RUN_SNAPSHOTS);
    }
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use topagent_core::BehaviorContract;

use crate::managed_files::write_managed_file;

pub(crate) const TOPAGENT_DIR: &str = ".topagent";
pub(crate) const WORKSPACE_STATE_RELATIVE_PATH: &str = ".topagent/workspace-state.json";
pub(crate) const CURRENT_WORKSPACE_SCHEMA_VERSION: u32 = 1;
const CURRENT_WORKSPACE_STATE_MODEL: &str = "topagent-workspace-state-v1";

pub(crate) const USER_PROFILE_RELATIVE_PATH: &str = ".topagent/USER.md";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_NOTES_RELATIVE_DIR: &str = ".topagent/notes";
pub(crate) const MEMORY_PROCEDURES_RELATIVE_DIR: &str = ".topagent/procedures";
pub(crate) const MEMORY_TRAJECTORIES_RELATIVE_DIR: &str = ".topagent/trajectories";
pub(crate) const TRAJECTORY_EXPORTS_RELATIVE_DIR: &str = ".topagent/exports/trajectories";
pub(crate) const RUN_SNAPSHOTS_RELATIVE_DIR: &str = ".topagent/run-snapshots";
pub(crate) const TELEGRAM_HISTORY_RELATIVE_DIR: &str = ".topagent/telegram-history";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceStatePathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceStateRole {
    SchemaMarker,
    HotPromptMemory,
    LazyPromptMemory,
    GovernedProcedure,
    EvidenceExport,
    RunSnapshot,
    TransportEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupportedWorkspacePath {
    pub(crate) relative_path: &'static str,
    pub(crate) kind: WorkspaceStatePathKind,
    pub(crate) role: WorkspaceStateRole,
    pub(crate) required: bool,
    pub(crate) prompt_loaded_by_default: bool,
}

pub(crate) const SUPPORTED_WORKSPACE_STATE_PATHS: &[SupportedWorkspacePath] = &[
    SupportedWorkspacePath {
        relative_path: WORKSPACE_STATE_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::File,
        role: WorkspaceStateRole::SchemaMarker,
        required: true,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: USER_PROFILE_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::File,
        role: WorkspaceStateRole::HotPromptMemory,
        required: false,
        prompt_loaded_by_default: true,
    },
    SupportedWorkspacePath {
        relative_path: MEMORY_INDEX_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::File,
        role: WorkspaceStateRole::HotPromptMemory,
        required: true,
        prompt_loaded_by_default: true,
    },
    SupportedWorkspacePath {
        relative_path: MEMORY_NOTES_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::LazyPromptMemory,
        required: true,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: MEMORY_PROCEDURES_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::GovernedProcedure,
        required: true,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: MEMORY_TRAJECTORIES_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::EvidenceExport,
        required: true,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: TRAJECTORY_EXPORTS_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::EvidenceExport,
        required: true,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: RUN_SNAPSHOTS_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::RunSnapshot,
        required: false,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: TELEGRAM_HISTORY_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::TransportEvidence,
        required: false,
        prompt_loaded_by_default: false,
    },
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceStateEnsureReport {
    pub(crate) schema_version: u32,
    pub(crate) created_schema_marker: bool,
    pub(crate) created_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceStateInspection {
    pub(crate) topagent_exists: bool,
    pub(crate) schema_version: Option<u32>,
    pub(crate) schema_error: Option<String>,
    pub(crate) missing_required_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedWorkspaceState {
    schema_version: u32,
    state_model: String,
}

pub(crate) fn ensure_workspace_state(workspace_root: &Path) -> Result<WorkspaceStateEnsureReport> {
    let mut report = WorkspaceStateEnsureReport {
        schema_version: CURRENT_WORKSPACE_SCHEMA_VERSION,
        ..WorkspaceStateEnsureReport::default()
    };

    ensure_dir(workspace_root, TOPAGENT_DIR, &mut report)?;
    ensure_schema_marker(workspace_root, &mut report)?;
    ensure_dir(workspace_root, MEMORY_NOTES_RELATIVE_DIR, &mut report)?;
    ensure_dir(workspace_root, MEMORY_PROCEDURES_RELATIVE_DIR, &mut report)?;
    ensure_dir(
        workspace_root,
        MEMORY_TRAJECTORIES_RELATIVE_DIR,
        &mut report,
    )?;
    ensure_dir(workspace_root, TRAJECTORY_EXPORTS_RELATIVE_DIR, &mut report)?;

    ensure_memory_index(workspace_root)?;

    Ok(report)
}

pub(crate) fn inspect_workspace_state(workspace_root: &Path) -> WorkspaceStateInspection {
    let topagent_dir = workspace_root.join(TOPAGENT_DIR);
    let topagent_exists = topagent_dir.is_dir();

    let (schema_version, schema_error) =
        match read_schema_marker(workspace_root).and_then(|marker| {
            marker
                .map(|marker| validate_schema_marker(workspace_root, marker))
                .transpose()
        }) {
            Ok(marker) => (marker.map(|marker| marker.schema_version), None),
            Err(err) => (None, Some(err.to_string())),
        };

    let missing_required_paths = SUPPORTED_WORKSPACE_STATE_PATHS
        .iter()
        .filter(|path| path.required)
        .filter(|path| !workspace_path_exists_as(workspace_root, path))
        .map(|path| path.relative_path.to_string())
        .collect::<Vec<_>>();

    WorkspaceStateInspection {
        topagent_exists,
        schema_version,
        schema_error,
        missing_required_paths,
    }
}

fn ensure_schema_marker(
    workspace_root: &Path,
    report: &mut WorkspaceStateEnsureReport,
) -> Result<()> {
    match read_schema_marker(workspace_root)? {
        Some(marker) if is_current_workspace_state_marker(&marker) => Ok(()),
        Some(marker) => anyhow::bail!(
            "unsupported workspace state model in {}: schema_version={}, state_model={}",
            workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH).display(),
            marker.schema_version,
            marker.state_model
        ),
        None => {
            let marker = PersistedWorkspaceState {
                schema_version: CURRENT_WORKSPACE_SCHEMA_VERSION,
                state_model: CURRENT_WORKSPACE_STATE_MODEL.to_string(),
            };
            let rendered = serde_json::to_string_pretty(&marker)
                .context("failed to serialize workspace state marker")?;
            write_managed_file(
                &workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH),
                &(rendered + "\n"),
                false,
            )?;
            report.created_schema_marker = true;
            Ok(())
        }
    }
}

fn read_schema_marker(workspace_root: &Path) -> Result<Option<PersistedWorkspaceState>> {
    let path = workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let marker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(marker))
}

fn validate_schema_marker(
    workspace_root: &Path,
    marker: PersistedWorkspaceState,
) -> Result<PersistedWorkspaceState> {
    if is_current_workspace_state_marker(&marker) {
        return Ok(marker);
    }

    anyhow::bail!(
        "unsupported workspace state model in {}: schema_version={}, state_model={}",
        workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH).display(),
        marker.schema_version,
        marker.state_model
    )
}

fn is_current_workspace_state_marker(marker: &PersistedWorkspaceState) -> bool {
    marker.schema_version == CURRENT_WORKSPACE_SCHEMA_VERSION
        && marker.state_model == CURRENT_WORKSPACE_STATE_MODEL
}

fn ensure_memory_index(workspace_root: &Path) -> Result<()> {
    let index_path = workspace_root.join(MEMORY_INDEX_RELATIVE_PATH);
    if index_path.exists() {
        return Ok(());
    }
    let template = BehaviorContract::default().render_memory_index_template();
    write_managed_file(&index_path, &template, false)
}

fn ensure_dir(
    workspace_root: &Path,
    relative: &str,
    report: &mut WorkspaceStateEnsureReport,
) -> Result<()> {
    let path = workspace_root.join(relative);
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    report.created_paths.push(relative.to_string());
    Ok(())
}

fn workspace_path_exists_as(workspace_root: &Path, supported: &SupportedWorkspacePath) -> bool {
    let path = workspace_root.join(supported.relative_path);
    match supported.kind {
        WorkspaceStatePathKind::File => path.is_file(),
        WorkspaceStatePathKind::Directory => path.is_dir(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ensure_workspace_state_creates_schema_marker_and_required_layout() {
        let temp = TempDir::new().unwrap();

        let report = ensure_workspace_state(temp.path()).unwrap();

        assert_eq!(report.schema_version, CURRENT_WORKSPACE_SCHEMA_VERSION);
        assert!(report.created_schema_marker);
        assert!(temp.path().join(WORKSPACE_STATE_RELATIVE_PATH).is_file());
        assert!(temp.path().join(MEMORY_INDEX_RELATIVE_PATH).is_file());
        assert!(temp.path().join(MEMORY_NOTES_RELATIVE_DIR).is_dir());
        assert!(temp.path().join(MEMORY_PROCEDURES_RELATIVE_DIR).is_dir());
        assert!(temp.path().join(MEMORY_TRAJECTORIES_RELATIVE_DIR).is_dir());
        assert!(temp.path().join(TRAJECTORY_EXPORTS_RELATIVE_DIR).is_dir());

        let inspection = inspect_workspace_state(temp.path());
        assert_eq!(
            inspection.schema_version,
            Some(CURRENT_WORKSPACE_SCHEMA_VERSION)
        );
        assert!(inspection.missing_required_paths.is_empty());
    }

    #[test]
    fn test_supported_workspace_state_paths_encode_prompt_boundaries() {
        let always_loaded = SUPPORTED_WORKSPACE_STATE_PATHS
            .iter()
            .filter(|path| path.prompt_loaded_by_default)
            .map(|path| path.relative_path)
            .collect::<Vec<_>>();
        assert_eq!(
            always_loaded,
            vec![USER_PROFILE_RELATIVE_PATH, MEMORY_INDEX_RELATIVE_PATH]
        );

        for evidence_path in [
            MEMORY_TRAJECTORIES_RELATIVE_DIR,
            TRAJECTORY_EXPORTS_RELATIVE_DIR,
            RUN_SNAPSHOTS_RELATIVE_DIR,
            TELEGRAM_HISTORY_RELATIVE_DIR,
        ] {
            let supported = SUPPORTED_WORKSPACE_STATE_PATHS
                .iter()
                .find(|path| path.relative_path == evidence_path)
                .unwrap();
            assert!(!supported.prompt_loaded_by_default);
        }
    }

    #[test]
    fn test_supported_workspace_state_paths_are_the_current_layout() {
        let paths = SUPPORTED_WORKSPACE_STATE_PATHS
            .iter()
            .map(|path| path.relative_path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                WORKSPACE_STATE_RELATIVE_PATH,
                USER_PROFILE_RELATIVE_PATH,
                MEMORY_INDEX_RELATIVE_PATH,
                MEMORY_NOTES_RELATIVE_DIR,
                MEMORY_PROCEDURES_RELATIVE_DIR,
                MEMORY_TRAJECTORIES_RELATIVE_DIR,
                TRAJECTORY_EXPORTS_RELATIVE_DIR,
                RUN_SNAPSHOTS_RELATIVE_DIR,
                TELEGRAM_HISTORY_RELATIVE_DIR,
            ]
        );
    }

    #[test]
    fn test_inspect_workspace_state_rejects_wrong_state_model() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(TOPAGENT_DIR)).unwrap();
        std::fs::write(
            temp.path().join(WORKSPACE_STATE_RELATIVE_PATH),
            r#"{
  "schema_version": 1,
  "state_model": "not-topagent"
}
"#,
        )
        .unwrap();

        let inspection = inspect_workspace_state(temp.path());

        assert_eq!(inspection.schema_version, None);
        assert!(inspection
            .schema_error
            .as_deref()
            .unwrap_or_default()
            .contains("unsupported workspace state model"));
    }

    #[test]
    fn test_ensure_workspace_state_rejects_wrong_state_model() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(TOPAGENT_DIR)).unwrap();
        std::fs::write(
            temp.path().join(WORKSPACE_STATE_RELATIVE_PATH),
            r#"{
  "schema_version": 1,
  "state_model": "not-topagent"
}
"#,
        )
        .unwrap();

        let err = ensure_workspace_state(temp.path()).unwrap_err().to_string();

        assert!(err.contains("unsupported workspace state model"));
    }
}

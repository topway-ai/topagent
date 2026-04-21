use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use topagent_core::{migrate_legacy_operator_preferences, BehaviorContract};

use crate::managed_files::write_managed_file;

pub(crate) const TOPAGENT_DIR: &str = ".topagent";
pub(crate) const WORKSPACE_STATE_RELATIVE_PATH: &str = ".topagent/workspace-state.json";
pub(crate) const CURRENT_WORKSPACE_SCHEMA_VERSION: u32 = 1;

pub(crate) const USER_PROFILE_RELATIVE_PATH: &str = ".topagent/USER.md";
pub(crate) const MEMORY_INDEX_RELATIVE_PATH: &str = ".topagent/MEMORY.md";
pub(crate) const MEMORY_NOTES_RELATIVE_DIR: &str = ".topagent/notes";
pub(crate) const MEMORY_PROCEDURES_RELATIVE_DIR: &str = ".topagent/procedures";
pub(crate) const MEMORY_TRAJECTORIES_RELATIVE_DIR: &str = ".topagent/trajectories";
pub(crate) const TRAJECTORY_EXPORTS_RELATIVE_DIR: &str = ".topagent/exports/trajectories";
pub(crate) const LEGACY_PLANS_EXPORT_RELATIVE_DIR: &str = ".topagent/exports/legacy-plans";
pub(crate) const CHECKPOINTS_RELATIVE_DIR: &str = ".topagent/checkpoints";
pub(crate) const TELEGRAM_HISTORY_RELATIVE_DIR: &str = ".topagent/telegram-history";
pub(crate) const HOOKS_MANIFEST_RELATIVE_PATH: &str = ".topagent/hooks.toml";
pub(crate) const EXTERNAL_TOOLS_RELATIVE_PATH: &str = ".topagent/external-tools.json";
pub(crate) const TOOLS_DIR_RELATIVE_PATH: &str = ".topagent/tools";

const LEGACY_TOPICS_RELATIVE_DIR: &str = ".topagent/topics";
const LEGACY_LESSONS_RELATIVE_DIR: &str = ".topagent/lessons";
const LEGACY_PLANS_RELATIVE_DIR: &str = ".topagent/plans";

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
    RunRecovery,
    TransportEvidence,
    RuntimeConfig,
    ToolSurface,
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
        relative_path: LEGACY_PLANS_EXPORT_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::EvidenceExport,
        required: false,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: CHECKPOINTS_RELATIVE_DIR,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::RunRecovery,
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
    SupportedWorkspacePath {
        relative_path: HOOKS_MANIFEST_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::File,
        role: WorkspaceStateRole::RuntimeConfig,
        required: false,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: EXTERNAL_TOOLS_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::File,
        role: WorkspaceStateRole::ToolSurface,
        required: false,
        prompt_loaded_by_default: false,
    },
    SupportedWorkspacePath {
        relative_path: TOOLS_DIR_RELATIVE_PATH,
        kind: WorkspaceStatePathKind::Directory,
        role: WorkspaceStateRole::ToolSurface,
        required: false,
        prompt_loaded_by_default: false,
    },
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceStateMigrationReport {
    pub(crate) schema_version: u32,
    pub(crate) created_schema_marker: bool,
    pub(crate) created_paths: Vec<String>,
    pub(crate) migrated_topic_files: usize,
    pub(crate) migrated_lesson_files: usize,
    pub(crate) migrated_plan_files: usize,
    pub(crate) migrated_operator_preferences: usize,
    pub(crate) removed_legacy_operator_files: usize,
    pub(crate) removed_legacy_index_entries: usize,
    pub(crate) rewritten_index_entries: usize,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceStateInspection {
    pub(crate) topagent_exists: bool,
    pub(crate) schema_version: Option<u32>,
    pub(crate) schema_error: Option<String>,
    pub(crate) missing_required_paths: Vec<String>,
    pub(crate) legacy_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedWorkspaceState {
    schema_version: u32,
    state_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathMigration {
    old_relative: String,
    new_relative: String,
}

pub(crate) fn ensure_workspace_state(
    workspace_root: &Path,
) -> Result<WorkspaceStateMigrationReport> {
    let mut report = WorkspaceStateMigrationReport {
        schema_version: CURRENT_WORKSPACE_SCHEMA_VERSION,
        ..WorkspaceStateMigrationReport::default()
    };

    ensure_dir(workspace_root, TOPAGENT_DIR, &mut report)?;
    reject_unsupported_future_schema(workspace_root)?;
    ensure_dir(workspace_root, MEMORY_NOTES_RELATIVE_DIR, &mut report)?;
    ensure_dir(workspace_root, MEMORY_PROCEDURES_RELATIVE_DIR, &mut report)?;
    ensure_dir(
        workspace_root,
        MEMORY_TRAJECTORIES_RELATIVE_DIR,
        &mut report,
    )?;
    ensure_dir(workspace_root, TRAJECTORY_EXPORTS_RELATIVE_DIR, &mut report)?;

    let operator_report = migrate_legacy_operator_preferences(workspace_root)
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    report.migrated_operator_preferences = operator_report.migrated_preferences;
    report.removed_legacy_operator_files = operator_report.removed_legacy_files;
    report.removed_legacy_index_entries = operator_report.removed_legacy_index_entries;

    let path_migrations = migrate_legacy_note_dirs(workspace_root, &mut report)?;
    report.rewritten_index_entries = rewrite_memory_index_paths(workspace_root, &path_migrations)?;
    migrate_legacy_plans(workspace_root, &mut report)?;

    ensure_memory_index(workspace_root)?;
    ensure_schema_marker(workspace_root, &mut report)?;

    Ok(report)
}

pub(crate) fn inspect_workspace_state(workspace_root: &Path) -> WorkspaceStateInspection {
    let topagent_dir = workspace_root.join(TOPAGENT_DIR);
    let topagent_exists = topagent_dir.is_dir();

    let (schema_version, schema_error) = match read_schema_marker(workspace_root) {
        Ok(marker) => (marker.map(|marker| marker.schema_version), None),
        Err(err) => (None, Some(err.to_string())),
    };

    let missing_required_paths = SUPPORTED_WORKSPACE_STATE_PATHS
        .iter()
        .filter(|path| path.required)
        .filter(|path| !workspace_path_exists_as(workspace_root, path))
        .map(|path| path.relative_path.to_string())
        .collect::<Vec<_>>();

    let legacy_paths = [
        LEGACY_TOPICS_RELATIVE_DIR,
        LEGACY_LESSONS_RELATIVE_DIR,
        LEGACY_PLANS_RELATIVE_DIR,
    ]
    .iter()
    .filter(|relative| workspace_root.join(relative).exists())
    .map(|relative| relative.to_string())
    .collect();

    WorkspaceStateInspection {
        topagent_exists,
        schema_version,
        schema_error,
        missing_required_paths,
        legacy_paths,
    }
}

fn reject_unsupported_future_schema(workspace_root: &Path) -> Result<()> {
    if let Some(marker) = read_schema_marker(workspace_root)? {
        if marker.schema_version > CURRENT_WORKSPACE_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported workspace schema version {} in {}; this TopAgent supports up to {}",
                marker.schema_version,
                workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH).display(),
                CURRENT_WORKSPACE_SCHEMA_VERSION
            );
        }
    }
    Ok(())
}

fn ensure_schema_marker(
    workspace_root: &Path,
    report: &mut WorkspaceStateMigrationReport,
) -> Result<()> {
    match read_schema_marker(workspace_root)? {
        Some(marker) if marker.schema_version > CURRENT_WORKSPACE_SCHEMA_VERSION => {
            anyhow::bail!(
                "unsupported workspace schema version {} in {}; this TopAgent supports up to {}",
                marker.schema_version,
                workspace_root.join(WORKSPACE_STATE_RELATIVE_PATH).display(),
                CURRENT_WORKSPACE_SCHEMA_VERSION
            );
        }
        Some(marker) if marker.schema_version == CURRENT_WORKSPACE_SCHEMA_VERSION => Ok(()),
        _ => {
            let marker = PersistedWorkspaceState {
                schema_version: CURRENT_WORKSPACE_SCHEMA_VERSION,
                state_model: "topagent-workspace-state-v1".to_string(),
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
    report: &mut WorkspaceStateMigrationReport,
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

fn migrate_legacy_note_dirs(
    workspace_root: &Path,
    report: &mut WorkspaceStateMigrationReport,
) -> Result<Vec<PathMigration>> {
    let mut migrations = Vec::new();
    migrations.extend(migrate_flat_files(
        workspace_root,
        LEGACY_TOPICS_RELATIVE_DIR,
        MEMORY_NOTES_RELATIVE_DIR,
        "topics",
        &mut report.migrated_topic_files,
        &mut report.warnings,
    )?);
    migrations.extend(migrate_flat_files(
        workspace_root,
        LEGACY_LESSONS_RELATIVE_DIR,
        MEMORY_NOTES_RELATIVE_DIR,
        "lessons",
        &mut report.migrated_lesson_files,
        &mut report.warnings,
    )?);
    Ok(migrations)
}

fn migrate_legacy_plans(
    workspace_root: &Path,
    report: &mut WorkspaceStateMigrationReport,
) -> Result<()> {
    let migrations = migrate_flat_files(
        workspace_root,
        LEGACY_PLANS_RELATIVE_DIR,
        LEGACY_PLANS_EXPORT_RELATIVE_DIR,
        "plans",
        &mut report.migrated_plan_files,
        &mut report.warnings,
    )?;
    if !migrations.is_empty() {
        ensure_dir(workspace_root, LEGACY_PLANS_EXPORT_RELATIVE_DIR, report)?;
    }
    Ok(())
}

fn migrate_flat_files(
    workspace_root: &Path,
    legacy_relative_dir: &str,
    destination_relative_dir: &str,
    legacy_label: &str,
    migrated_count: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<PathMigration>> {
    let legacy_dir = workspace_root.join(legacy_relative_dir);
    if !legacy_dir.is_dir() {
        return Ok(Vec::new());
    }

    let destination_dir = workspace_root.join(destination_relative_dir);
    std::fs::create_dir_all(&destination_dir)
        .with_context(|| format!("failed to create {}", destination_dir.display()))?;

    let mut migrations = Vec::new();
    let mut entries = std::fs::read_dir(&legacy_dir)
        .with_context(|| format!("failed to read {}", legacy_dir.display()))?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let src = entry.path();
        if !src.is_file() {
            warnings.push(format!(
                "left legacy {} entry in place because it is not a file: {}",
                legacy_label,
                src.display()
            ));
            continue;
        }

        let file_name = entry.file_name();
        let destination = choose_destination(&src, &destination_dir, &file_name, legacy_label)?;
        let old_relative = format!(
            "{}/{}",
            legacy_relative_dir.trim_start_matches(".topagent/"),
            file_name.to_string_lossy()
        );
        let new_relative = format!(
            "{}/{}",
            destination_relative_dir.trim_start_matches(".topagent/"),
            destination
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        );

        move_file(&src, &destination)?;
        migrations.push(PathMigration {
            old_relative,
            new_relative,
        });
        *migrated_count += 1;
    }

    if let Err(err) = std::fs::remove_dir(&legacy_dir) {
        if legacy_dir.exists() {
            warnings.push(format!(
                "left legacy {} directory in place ({}): {}",
                legacy_label,
                legacy_dir.display(),
                err
            ));
        }
    }

    Ok(migrations)
}

fn choose_destination(
    src: &Path,
    destination_dir: &Path,
    file_name: &std::ffi::OsStr,
    legacy_label: &str,
) -> Result<PathBuf> {
    let destination = destination_dir.join(file_name);
    if !destination.exists() || files_match(src, &destination)? {
        return Ok(destination);
    }

    let original = Path::new(file_name);
    let stem = original
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("legacy");
    let extension = original.extension().and_then(|ext| ext.to_str());

    for idx in 1..100 {
        let candidate_name = match extension {
            Some(ext) => format!("{legacy_label}-{stem}-{idx}.{ext}"),
            None => format!("{legacy_label}-{stem}-{idx}"),
        };
        let candidate = destination_dir.join(candidate_name);
        if !candidate.exists() || files_match(src, &candidate)? {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "could not find a collision-free destination for legacy file {}",
        src.display()
    );
}

fn files_match(left: &Path, right: &Path) -> Result<bool> {
    if !left.exists() || !right.exists() {
        return Ok(false);
    }
    Ok(
        std::fs::read(left).with_context(|| format!("failed to read {}", left.display()))?
            == std::fs::read(right)
                .with_context(|| format!("failed to read {}", right.display()))?,
    )
}

fn move_file(src: &Path, destination: &Path) -> Result<()> {
    if destination.exists() && files_match(src, destination)? {
        std::fs::remove_file(src)
            .with_context(|| format!("failed to remove duplicate legacy file {}", src.display()))?;
        return Ok(());
    }

    match std::fs::rename(src, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(src, destination).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src.display(),
                    destination.display()
                )
            })?;
            std::fs::remove_file(src)
                .with_context(|| format!("failed to remove migrated legacy file {}", src.display()))
        }
    }
}

fn rewrite_memory_index_paths(
    workspace_root: &Path,
    migrations: &[PathMigration],
) -> Result<usize> {
    if migrations.is_empty() {
        return Ok(0);
    }
    let index_path = workspace_root.join(MEMORY_INDEX_RELATIVE_PATH);
    if !index_path.exists() {
        return Ok(0);
    }

    let raw = std::fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    let migration_map = migrations
        .iter()
        .flat_map(|migration| {
            [
                (
                    migration.old_relative.clone(),
                    migration.new_relative.clone(),
                ),
                (
                    format!(".topagent/{}", migration.old_relative),
                    migration.new_relative.clone(),
                ),
            ]
        })
        .collect::<BTreeMap<_, _>>();

    let mut rewritten = Vec::new();
    let mut changed = 0usize;
    for line in raw.lines() {
        let mut next = line.to_string();
        for (old, new) in &migration_map {
            let needle = format!("file: {old}");
            if next.contains(&needle) {
                next = next.replace(&needle, &format!("file: {new}"));
            }
        }
        if next != line {
            changed += 1;
        }
        rewritten.push(next);
    }

    if changed > 0 {
        let mut rendered = rewritten.join("\n");
        rendered.push('\n');
        write_managed_file(&index_path, &rendered, false)?;
    }

    Ok(changed)
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
            LEGACY_PLANS_EXPORT_RELATIVE_DIR,
            CHECKPOINTS_RELATIVE_DIR,
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
    fn test_ensure_workspace_state_migrates_legacy_topics_lessons_and_index_paths() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(TOPAGENT_DIR);
        std::fs::create_dir_all(root.join("topics")).unwrap();
        std::fs::create_dir_all(root.join("lessons")).unwrap();
        std::fs::write(root.join("topics/architecture.md"), "# Architecture").unwrap();
        std::fs::write(root.join("lessons/deploy.md"), "# Deploy").unwrap();
        std::fs::write(
            root.join("MEMORY.md"),
            "# TopAgent Memory Index\n\n- topic: architecture | file: topics/architecture.md | status: verified | note: runtime\n- topic: deploy | file: lessons/deploy.md | status: verified | note: release\n",
        )
        .unwrap();

        let report = ensure_workspace_state(temp.path()).unwrap();

        assert_eq!(report.migrated_topic_files, 1);
        assert_eq!(report.migrated_lesson_files, 1);
        assert_eq!(report.rewritten_index_entries, 2);
        assert!(root.join("notes/architecture.md").is_file());
        assert!(root.join("notes/deploy.md").is_file());
        assert!(!root.join("topics/architecture.md").exists());
        assert!(!root.join("lessons/deploy.md").exists());
        let index = std::fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert!(index.contains("file: notes/architecture.md"));
        assert!(index.contains("file: notes/deploy.md"));
        assert!(!index.contains("file: topics/"));
        assert!(!index.contains("file: lessons/"));
    }

    #[test]
    fn test_ensure_workspace_state_moves_legacy_plans_to_export_evidence() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(TOPAGENT_DIR);
        std::fs::create_dir_all(root.join("plans")).unwrap();
        std::fs::write(root.join("plans/refactor.md"), "# Temporary plan").unwrap();

        let report = ensure_workspace_state(temp.path()).unwrap();

        assert_eq!(report.migrated_plan_files, 1);
        assert!(root.join("exports/legacy-plans/refactor.md").is_file());
        assert!(!root.join("plans/refactor.md").exists());
    }
}

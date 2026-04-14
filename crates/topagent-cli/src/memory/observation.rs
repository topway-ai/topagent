use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use topagent_core::{RunTrustContext, SourceLabel};

use super::compact_text_line;
use super::promotion::TaskPromotionReport;

use crate::managed_files::write_managed_file;

const MAX_OBSERVATION_SCAN: usize = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationSourceKind {
    Lesson,
    Procedure,
    Trajectory,
    LessonAndProcedure,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationTrustClass {
    Trusted,
    AdvisoryOnly,
    LowTrustPresent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ObservationArtifactLinks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lesson_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub procedure_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_procedure_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ObservationRecord {
    pub id: String,
    pub timestamp_unix_secs: i64,
    pub task_intent: String,
    pub source_kind: ObservationSourceKind,
    pub trust_class: ObservationTrustClass,
    pub summary: String,
    pub artifact_links: ObservationArtifactLinks,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_command: Option<String>,
}

// ── Loading ──

pub(crate) fn emit_observation(
    observations_dir: &Path,
    report: &TaskPromotionReport,
    instruction: &str,
    source_labels: &[SourceLabel],
    changed_files: &[String],
    verification_command: Option<&str>,
) -> Result<Option<String>> {
    if report.lesson_file.is_none()
        && report.procedure_file.is_none()
        && report.trajectory_file.is_none()
    {
        return Ok(None);
    }

    std::fs::create_dir_all(observations_dir)
        .with_context(|| format!("failed to create {}", observations_dir.display()))?;

    let timestamp = unix_timestamp_secs();
    let id = format!("obs-{}-{}", timestamp, slugify(instruction));
    let filename = format!("{id}.json");
    let path = observations_dir.join(&filename);

    let source_kind = classify_source_kind(report);
    let trust_class = classify_trust_class(source_labels);
    let summary = build_observation_summary(instruction, report);

    let record = ObservationRecord {
        id: id.clone(),
        timestamp_unix_secs: timestamp,
        task_intent: compact_text_line(instruction, 220),
        source_kind,
        trust_class,
        summary,
        artifact_links: ObservationArtifactLinks {
            lesson_file: report.lesson_file.clone(),
            procedure_file: report.procedure_file.clone(),
            trajectory_file: report.trajectory_file.clone(),
            superseded_procedure_file: report.superseded_procedure_file.clone(),
        },
        changed_files: changed_files.iter().take(12).cloned().collect(),
        verification_command: verification_command.map(|cmd| compact_text_line(cmd, 120)),
    };

    let json = serde_json::to_string_pretty(&record)
        .with_context(|| format!("failed to encode observation {id}"))?;
    write_managed_file(&path, &json, false)?;

    Ok(Some(format!(".topagent/observations/{filename}")))
}

fn classify_source_kind(report: &TaskPromotionReport) -> ObservationSourceKind {
    let has_lesson = report.lesson_file.is_some();
    let has_procedure = report.procedure_file.is_some();
    let has_trajectory = report.trajectory_file.is_some();

    match (has_lesson, has_procedure, has_trajectory) {
        (_, _, true) => ObservationSourceKind::Full,
        (true, true, false) => ObservationSourceKind::LessonAndProcedure,
        (_, true, false) => ObservationSourceKind::Procedure,
        (true, false, false) => ObservationSourceKind::Lesson,
        (false, false, false) => ObservationSourceKind::Lesson, // unreachable given guard above
    }
}

fn classify_trust_class(source_labels: &[SourceLabel]) -> ObservationTrustClass {
    let trust_context = RunTrustContext {
        sources: source_labels.to_vec(),
    };
    if trust_context.has_low_trust_action_influence() {
        ObservationTrustClass::LowTrustPresent
    } else if trust_context.has_low_trust_sources() {
        ObservationTrustClass::AdvisoryOnly
    } else {
        ObservationTrustClass::Trusted
    }
}

fn build_observation_summary(instruction: &str, report: &TaskPromotionReport) -> String {
    let mut parts = Vec::new();
    if report.procedure_file.is_some() {
        parts.push("procedure");
    }
    if report.lesson_file.is_some() {
        parts.push("lesson");
    }
    if report.trajectory_file.is_some() {
        parts.push("trajectory");
    }
    let promoted = parts.join("+");
    compact_text_line(&format!("[{promoted}] {}", instruction.trim()), 160)
}

// ── Loading ──

pub(crate) fn load_observation(path: &Path) -> Result<Option<ObservationRecord>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let record = serde_json::from_str(&raw)
        .with_context(|| format!("failed to decode {}", path.display()))?;
    Ok(Some(record))
}

pub(crate) fn scan_observations(
    observations_dir: &Path,
    limit: usize,
) -> Result<Vec<ObservationRecord>> {
    if !observations_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(observations_dir)
        .with_context(|| format!("failed to read {}", observations_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();

    // Sort by filename descending (newest first, since filenames contain timestamps)
    paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    paths.truncate(limit.min(MAX_OBSERVATION_SCAN));

    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        match load_observation(&path) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("skipping malformed observation {}: {err}", path.display());
            }
        }
    }

    Ok(records)
}

impl ObservationSourceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Lesson => "lesson",
            Self::Procedure => "procedure",
            Self::Trajectory => "trajectory",
            Self::LessonAndProcedure => "lesson+procedure",
            Self::Full => "full",
        }
    }
}

impl ObservationTrustClass {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::AdvisoryOnly => "advisory",
            Self::LowTrustPresent => "low-trust",
        }
    }
}

// ── CLI Rendering ──

pub(crate) fn render_observation_list(observations: &[ObservationRecord]) -> String {
    if observations.is_empty() {
        return "No observations found.".to_string();
    }

    let mut output = String::new();
    for obs in observations {
        output.push_str(&format!(
            "{} [{}] [{}] {}\n",
            obs.id,
            obs.trust_class.label(),
            obs.source_kind.label(),
            compact_text_line(&obs.task_intent, 80),
        ));
    }
    output
}

pub(crate) fn render_observation_detail(obs: &ObservationRecord) -> String {
    let mut output = String::new();
    output.push_str(&format!("ID:           {}\n", obs.id));
    output.push_str(&format!("Timestamp:    {}\n", obs.timestamp_unix_secs));
    output.push_str(&format!("Task Intent:  {}\n", obs.task_intent));
    output.push_str(&format!("Source Kind:  {}\n", obs.source_kind.label()));
    output.push_str(&format!("Trust Class:  {}\n", obs.trust_class.label()));
    output.push_str(&format!("Summary:      {}\n", obs.summary));

    output.push_str("\nArtifact Links:\n");
    if let Some(ref path) = obs.artifact_links.lesson_file {
        output.push_str(&format!("  Lesson:     {path}\n"));
    }
    if let Some(ref path) = obs.artifact_links.procedure_file {
        output.push_str(&format!("  Procedure:  {path}\n"));
    }
    if let Some(ref path) = obs.artifact_links.trajectory_file {
        output.push_str(&format!("  Trajectory: {path}\n"));
    }
    if let Some(ref path) = obs.artifact_links.superseded_procedure_file {
        output.push_str(&format!("  Superseded: {path}\n"));
    }

    if !obs.changed_files.is_empty() {
        output.push_str("\nChanged Files:\n");
        for file in &obs.changed_files {
            output.push_str(&format!("  - {file}\n"));
        }
    }

    if let Some(ref cmd) = obs.verification_command {
        output.push_str(&format!("\nVerification: {cmd}\n"));
    }

    output
}

// ── Helpers ──

fn slugify(input: &str) -> String {
    let slug: String = input
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == ' ' || *ch == '-')
        .take(48)
        .collect::<String>()
        .replace(' ', "-");
    if slug.is_empty() {
        "observation".to_string()
    } else {
        slug
    }
}

fn unix_timestamp_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;
    use topagent_core::{InfluenceMode, SourceKind};

    pub(crate) fn current_timestamp() -> i64 {
        unix_timestamp_secs()
    }

    fn sample_report() -> TaskPromotionReport {
        TaskPromotionReport {
            lesson_file: Some(".topagent/lessons/1234-fix-parsing.md".to_string()),
            procedure_file: Some(".topagent/procedures/1234-fix-parsing.md".to_string()),
            trajectory_file: Some(".topagent/trajectories/trj-1234-fix-parsing.json".to_string()),
            superseded_procedure_file: None,
            notes: Vec::new(),
        }
    }

    fn trusted_labels() -> Vec<SourceLabel> {
        vec![SourceLabel::trusted(
            SourceKind::OperatorDirect,
            InfluenceMode::MayDriveAction,
            "operator instruction",
        )]
    }

    fn low_trust_labels() -> Vec<SourceLabel> {
        vec![
            SourceLabel::trusted(
                SourceKind::OperatorDirect,
                InfluenceMode::MayDriveAction,
                "operator instruction",
            ),
            SourceLabel::low(
                SourceKind::FetchedWebContent,
                InfluenceMode::MayDriveAction,
                "curl https://example.com",
            ),
        ]
    }

    #[test]
    fn test_emit_observation_creates_file() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        let report = sample_report();
        let result = emit_observation(
            &obs_dir,
            &report,
            "Fix the parsing bug in config.rs",
            &trusted_labels(),
            &["src/config.rs".to_string()],
            Some("cargo test"),
        )
        .unwrap();

        assert!(result.is_some());
        let path_str = result.unwrap();
        assert!(path_str.starts_with(".topagent/observations/obs-"));
        assert!(path_str.ends_with(".json"));

        let full_path = temp.path().join(path_str.trim_start_matches(".topagent/"));
        let record = load_observation(&full_path).unwrap().unwrap();
        assert_eq!(record.source_kind, ObservationSourceKind::Full);
        assert_eq!(record.trust_class, ObservationTrustClass::Trusted);
        assert!(record.task_intent.contains("parsing bug"));
    }

    #[test]
    fn test_emit_observation_returns_none_for_empty_report() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        let report = TaskPromotionReport::default();
        let result = emit_observation(
            &obs_dir,
            &report,
            "some instruction",
            &trusted_labels(),
            &[],
            None,
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_classify_source_kind() {
        let report = TaskPromotionReport {
            lesson_file: Some("l.md".to_string()),
            ..Default::default()
        };
        assert_eq!(classify_source_kind(&report), ObservationSourceKind::Lesson);

        let report = TaskPromotionReport {
            lesson_file: Some("l.md".to_string()),
            procedure_file: Some("p.md".to_string()),
            ..Default::default()
        };
        assert_eq!(
            classify_source_kind(&report),
            ObservationSourceKind::LessonAndProcedure
        );

        let report = TaskPromotionReport {
            lesson_file: Some("l.md".to_string()),
            procedure_file: Some("p.md".to_string()),
            trajectory_file: Some("t.json".to_string()),
            ..Default::default()
        };
        assert_eq!(classify_source_kind(&report), ObservationSourceKind::Full);
    }

    #[test]
    fn test_classify_trust_class_trusted() {
        assert_eq!(
            classify_trust_class(&trusted_labels()),
            ObservationTrustClass::Trusted
        );
    }

    #[test]
    fn test_classify_trust_class_low_trust() {
        assert_eq!(
            classify_trust_class(&low_trust_labels()),
            ObservationTrustClass::LowTrustPresent
        );
    }

    #[test]
    fn test_scan_observations_capped() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        std::fs::create_dir_all(&obs_dir).unwrap();

        for i in 0..25 {
            let report = TaskPromotionReport {
                lesson_file: Some(format!(".topagent/lessons/{i}-lesson.md")),
                ..TaskPromotionReport::default()
            };
            // Write directly to avoid timestamp collisions
            let id = format!("obs-{:010}-task-{i}", 1000000 + i);
            let record = ObservationRecord {
                id: id.clone(),
                timestamp_unix_secs: 1000000 + i,
                task_intent: format!("task number {i}"),
                source_kind: ObservationSourceKind::Lesson,
                trust_class: ObservationTrustClass::Trusted,
                summary: format!("[lesson] task number {i}"),
                artifact_links: ObservationArtifactLinks {
                    lesson_file: report.lesson_file,
                    procedure_file: None,
                    trajectory_file: None,
                    superseded_procedure_file: None,
                },
                changed_files: vec![],
                verification_command: None,
            };
            let json = serde_json::to_string_pretty(&record).unwrap();
            std::fs::write(obs_dir.join(format!("{id}.json")), json).unwrap();
        }

        let results = scan_observations(&obs_dir, 10).unwrap();
        assert_eq!(results.len(), 10);
        // Newest first
        assert!(results[0].timestamp_unix_secs > results[9].timestamp_unix_secs);
    }

    #[test]
    fn test_observation_format_is_human_inspectable() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        let report = sample_report();
        let result = emit_observation(
            &obs_dir,
            &report,
            "Fix parsing bug",
            &trusted_labels(),
            &["src/parser.rs".to_string()],
            Some("cargo test -p parser"),
        )
        .unwrap()
        .unwrap();

        let full_path = temp.path().join(result.trim_start_matches(".topagent/"));
        let raw = std::fs::read_to_string(&full_path).unwrap();
        let record: ObservationRecord = serde_json::from_str(&raw).unwrap();

        assert!(!record.id.is_empty());
        assert!(record.task_intent.len() <= 220);
        assert!(record.summary.len() <= 160);
        assert!(!record.changed_files.is_empty());
        assert!(record.verification_command.is_some());
    }

    #[test]
    fn test_render_observation_list_format() {
        let obs = vec![ObservationRecord {
            id: "obs-123-test".to_string(),
            timestamp_unix_secs: 123,
            task_intent: "fix the bug".to_string(),
            source_kind: ObservationSourceKind::Lesson,
            trust_class: ObservationTrustClass::Trusted,
            summary: "[lesson] fix the bug".to_string(),
            artifact_links: ObservationArtifactLinks {
                lesson_file: Some("l.md".to_string()),
                procedure_file: None,
                trajectory_file: None,
                superseded_procedure_file: None,
            },
            changed_files: vec![],
            verification_command: None,
        }];

        let rendered = render_observation_list(&obs);
        assert!(rendered.contains("obs-123-test"));
        assert!(rendered.contains("[trusted]"));
        assert!(rendered.contains("[lesson]"));
    }
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use topagent_core::{RunTrustContext, SourceLabel};

use super::promotion::TaskPromotionReport;
use super::{compact_text_line, score_text_relevance};

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

#[derive(Debug, Clone, Default)]
pub(crate) struct RetrievalResult {
    pub candidates: Vec<ObservationRecord>,
    /// Temporal neighbors of the top candidate that share changed files or verification.
    /// Used during stage-3 artifact resolution and available for future expansion display.
    #[allow(dead_code)]
    pub expanded: Vec<ObservationRecord>,
    pub artifact_paths: Vec<String>,
    pub provenance_notes: Vec<String>,
}

// ── Emission ──

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
    compact_text_line(
        &format!("[{promoted}] {}", instruction.trim()),
        160,
    )
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
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "json")
        })
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

// ── Matching ──

pub(crate) fn match_observations<'a>(
    observations: &'a [ObservationRecord],
    instruction: &str,
    max_candidates: usize,
) -> Vec<&'a ObservationRecord> {
    let mut scored: Vec<(usize, &ObservationRecord)> = observations
        .iter()
        .filter_map(|obs| {
            let mut haystack = obs.task_intent.clone();
            haystack.push(' ');
            haystack.push_str(&obs.summary);
            for file in &obs.changed_files {
                haystack.push(' ');
                haystack.push_str(file);
            }
            let score = score_text_relevance(instruction, &haystack);
            // Recency bonus: observations from last 7 days get +1
            let recency_bonus = if obs.timestamp_unix_secs + 604_800 >= unix_timestamp_secs() {
                1
            } else {
                0
            };
            let total = score + recency_bonus;
            (total > 0).then_some((total, obs))
        })
        .collect();

    scored.sort_by(|(score_a, obs_a), (score_b, obs_b)| {
        score_b
            .cmp(score_a)
            .then_with(|| obs_b.timestamp_unix_secs.cmp(&obs_a.timestamp_unix_secs))
    });

    scored
        .into_iter()
        .take(max_candidates)
        .map(|(_, obs)| obs)
        .collect()
}

// ── Progressive Retrieval ──

pub(crate) fn progressive_retrieve(
    observations_dir: &Path,
    instruction: &str,
    max_candidates: usize,
    max_expansion: usize,
) -> Result<RetrievalResult> {
    if !observations_dir.is_dir() {
        return Ok(RetrievalResult::default());
    }

    // Stage 1: scan and match
    let all_observations = scan_observations(observations_dir, MAX_OBSERVATION_SCAN)?;
    if all_observations.is_empty() {
        return Ok(RetrievalResult::default());
    }

    let matched = match_observations(&all_observations, instruction, max_candidates);
    if matched.is_empty() {
        return Ok(RetrievalResult::default());
    }

    let candidates: Vec<ObservationRecord> = matched.into_iter().cloned().collect();
    let candidate_ids: HashSet<&str> = candidates.iter().map(|obs| obs.id.as_str()).collect();

    // Stage 2: expand temporal neighbors of top candidate
    let mut expanded = Vec::new();
    if let Some(top) = candidates.first() {
        let top_files: HashSet<&str> = top.changed_files.iter().map(|f| f.as_str()).collect();

        for obs in &all_observations {
            if candidate_ids.contains(obs.id.as_str()) {
                continue;
            }
            if expanded.len() >= max_expansion {
                break;
            }

            // Temporal proximity: within 2 positions in the sorted list
            let time_diff = (obs.timestamp_unix_secs - top.timestamp_unix_secs).unsigned_abs();
            let is_temporal_neighbor = time_diff <= 86_400; // within 1 day

            // File overlap
            let has_file_overlap = obs
                .changed_files
                .iter()
                .any(|f| top_files.contains(f.as_str()));

            // Same verification command
            let has_same_verification = top.verification_command.is_some()
                && obs.verification_command == top.verification_command;

            if is_temporal_neighbor && (has_file_overlap || has_same_verification) {
                expanded.push(obs.clone());
            }
        }
    }

    // Stage 3: collect artifact paths + provenance notes
    let mut artifact_paths = Vec::new();
    let mut provenance_notes = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    for obs in candidates.iter().chain(expanded.iter()) {
        // Skip low-trust observations for artifact boosting
        let is_low_trust = obs.trust_class == ObservationTrustClass::LowTrustPresent;

        for path in artifact_links_iter(&obs.artifact_links) {
            if seen_paths.insert(path.clone()) {
                if !is_low_trust {
                    artifact_paths.push(path.clone());
                }

                let trust_note = if is_low_trust {
                    " [low-trust, not boosted]"
                } else {
                    ""
                };
                if provenance_notes.len() < 4 {
                    provenance_notes.push(format!(
                        "{} -> {} ({}{})",
                        obs.id,
                        path,
                        obs.source_kind.label(),
                        trust_note,
                    ));
                }
            }
        }
    }

    Ok(RetrievalResult {
        candidates,
        expanded,
        artifact_paths,
        provenance_notes,
    })
}

fn artifact_links_iter(links: &ObservationArtifactLinks) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(ref path) = links.lesson_file {
        paths.push(path.clone());
    }
    if let Some(ref path) = links.procedure_file {
        paths.push(path.clone());
    }
    // Trajectories are off-prompt; include only for provenance tracing, not boosting
    if let Some(ref path) = links.trajectory_file {
        paths.push(path.clone());
    }
    paths
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
        let mut report = TaskPromotionReport::default();
        report.lesson_file = Some("l.md".to_string());
        assert_eq!(classify_source_kind(&report), ObservationSourceKind::Lesson);

        report.procedure_file = Some("p.md".to_string());
        assert_eq!(
            classify_source_kind(&report),
            ObservationSourceKind::LessonAndProcedure
        );

        report.trajectory_file = Some("t.json".to_string());
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
    fn test_match_observations_returns_bounded_candidates() {
        let observations: Vec<ObservationRecord> = (0..20)
            .map(|i| ObservationRecord {
                id: format!("obs-{i}"),
                timestamp_unix_secs: 1000000 + i,
                task_intent: format!("fix parsing bug in module {i}"),
                source_kind: ObservationSourceKind::Lesson,
                trust_class: ObservationTrustClass::Trusted,
                summary: format!("[lesson] fix parsing bug in module {i}"),
                artifact_links: ObservationArtifactLinks {
                    lesson_file: Some(format!("lessons/{i}.md")),
                    procedure_file: None,
                    trajectory_file: None,
                    superseded_procedure_file: None,
                },
                changed_files: vec![format!("src/module_{i}.rs")],
                verification_command: Some("cargo test".to_string()),
            })
            .collect();

        let matched = match_observations(&observations, "fix parsing bug", 5);
        assert!(matched.len() <= 5);
        assert!(!matched.is_empty());
    }

    #[test]
    fn test_progressive_retrieve_empty_dir() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        let result = progressive_retrieve(&obs_dir, "anything", 8, 4).unwrap();
        assert!(result.candidates.is_empty());
        assert!(result.artifact_paths.is_empty());
    }

    #[test]
    fn test_progressive_retrieve_excludes_low_trust_from_boosting() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        std::fs::create_dir_all(&obs_dir).unwrap();

        let record = ObservationRecord {
            id: "obs-1-low-trust-task".to_string(),
            timestamp_unix_secs: unix_timestamp_secs(),
            task_intent: "fix approval bug".to_string(),
            source_kind: ObservationSourceKind::Lesson,
            trust_class: ObservationTrustClass::LowTrustPresent,
            summary: "[lesson] fix approval bug".to_string(),
            artifact_links: ObservationArtifactLinks {
                lesson_file: Some(".topagent/lessons/low-trust-lesson.md".to_string()),
                procedure_file: None,
                trajectory_file: None,
                superseded_procedure_file: None,
            },
            changed_files: vec!["src/approval.rs".to_string()],
            verification_command: Some("cargo test".to_string()),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(obs_dir.join("obs-1-low-trust-task.json"), json).unwrap();

        let result = progressive_retrieve(&obs_dir, "fix approval bug", 8, 4).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(
            result.candidates[0].trust_class,
            ObservationTrustClass::LowTrustPresent
        );
        // Low-trust artifacts not in boosting paths
        assert!(result.artifact_paths.is_empty());
        // But provenance notes still mention it
        assert!(!result.provenance_notes.is_empty());
        assert!(result.provenance_notes[0].contains("low-trust"));
    }

    #[test]
    fn test_progressive_retrieve_resolves_artifact_paths() {
        let temp = TempDir::new().unwrap();
        let obs_dir = temp.path().join("observations");
        std::fs::create_dir_all(&obs_dir).unwrap();

        let record = ObservationRecord {
            id: "obs-1-approval-fix".to_string(),
            timestamp_unix_secs: unix_timestamp_secs(),
            task_intent: "harden the approval mailbox".to_string(),
            source_kind: ObservationSourceKind::LessonAndProcedure,
            trust_class: ObservationTrustClass::Trusted,
            summary: "[lesson+procedure] harden the approval mailbox".to_string(),
            artifact_links: ObservationArtifactLinks {
                lesson_file: Some(".topagent/lessons/approval-lesson.md".to_string()),
                procedure_file: Some(".topagent/procedures/approval-proc.md".to_string()),
                trajectory_file: None,
                superseded_procedure_file: None,
            },
            changed_files: vec!["src/approval.rs".to_string()],
            verification_command: Some("cargo test".to_string()),
        };
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(obs_dir.join("obs-1-approval-fix.json"), json).unwrap();

        let result = progressive_retrieve(&obs_dir, "harden the approval mailbox", 8, 4).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert!(result
            .artifact_paths
            .contains(&".topagent/lessons/approval-lesson.md".to_string()));
        assert!(result
            .artifact_paths
            .contains(&".topagent/procedures/approval-proc.md".to_string()));
        assert!(!result.provenance_notes.is_empty());
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

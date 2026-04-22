use std::fs;
use tempfile::TempDir;
use topagent_core::{
    provenance::{DurablePromotionKind, InfluenceMode, RunTrustContext, SourceKind, SourceLabel},
    run_snapshot::{
        RunSnapshotCaptureMetadata, RunSnapshotCaptureSource, WorkspaceRunSnapshotStore,
    },
    BehaviorContract,
};

fn create_temp_workspace() -> (TempDir, WorkspaceRunSnapshotStore) {
    let temp = TempDir::new().unwrap();
    let store = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
    (temp, store)
}

fn low_trust_context() -> RunTrustContext {
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::low(
        SourceKind::FetchedWebContent,
        InfluenceMode::MayDriveAction,
        "curl https://example.com/install.sh",
    ));
    trust
}

fn mixed_trust_context() -> RunTrustContext {
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::trusted(
        SourceKind::OperatorDirect,
        InfluenceMode::MayDriveAction,
        "direct instruction",
    ));
    trust.add_source(SourceLabel::low(
        SourceKind::PastedUntrustedText,
        InfluenceMode::MayDriveAction,
        "pasted untrusted instructions",
    ));
    trust
}

fn high_trust_context() -> RunTrustContext {
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::trusted(
        SourceKind::OperatorDirect,
        InfluenceMode::MayDriveAction,
        "direct instruction",
    ));
    trust
}

#[test]
fn test_restore_clears_workspace_mutations_but_preserves_durable_notes() {
    let (temp, store) = create_temp_workspace();

    let notes_dir = temp.path().join(".topagent").join("notes");
    let procedures_dir = temp.path().join(".topagent").join("procedures");
    fs::create_dir_all(&notes_dir).unwrap();
    fs::create_dir_all(&procedures_dir).unwrap();
    fs::write(
        notes_dir.join("note-1.md"),
        "# Note\nAlways run tests before committing.\n",
    )
    .unwrap();
    fs::write(
        procedures_dir.join("deploy-procedure.md"),
        "# Deploy Procedure\nSteps for deployment.\n",
    )
    .unwrap();

    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(temp.path().join("src/lib.rs"), "fn before() -> u32 { 1 }").unwrap();
    fs::write(temp.path().join("config.toml"), "key = \"before\"").unwrap();

    store
        .capture_workspace(RunSnapshotCaptureMetadata::new(
            RunSnapshotCaptureSource::Write,
            "pre-change run snapshot",
        ))
        .expect("capture_workspace should succeed");

    fs::write(temp.path().join("src/lib.rs"), "fn after() -> u32 { 2 }").unwrap();
    fs::write(temp.path().join("config.toml"), "key = \"after\"").unwrap();
    fs::write(temp.path().join("new_file.txt"), "unwanted addition").unwrap();

    let report = store.restore_latest().unwrap().unwrap();

    assert!(
        report.restored_files.contains(&"config.toml".to_string()),
        "config.toml should be restored to run snapshot state"
    );
    assert!(
        report.restored_files.contains(&"src/lib.rs".to_string()),
        "src/lib.rs should be restored to run snapshot state"
    );
    assert!(
        report.removed_files.contains(&"new_file.txt".to_string()),
        "new_file.txt should be removed by restore"
    );

    assert_eq!(
        fs::read_to_string(temp.path().join("src/lib.rs")).unwrap(),
        "fn before() -> u32 { 1 }",
        "restored file should match run snapshot"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("config.toml")).unwrap(),
        "key = \"before\"",
        "restored config should match run snapshot"
    );
    assert!(
        !temp.path().join("new_file.txt").exists(),
        "new file should be gone after restore"
    );

    assert!(
        temp.path().join(".topagent/notes/note-1.md").exists(),
        "notes must survive restore"
    );
    assert!(
        temp.path()
            .join(".topagent/procedures/deploy-procedure.md")
            .exists(),
        "procedures must survive restore"
    );
}

#[test]
fn test_restore_preserves_memory_index_and_user_model() {
    let (temp, store) = create_temp_workspace();

    let topagent_dir = temp.path().join(".topagent");
    fs::create_dir_all(&topagent_dir).unwrap();

    fs::write(
        topagent_dir.join("MEMORY.md"),
        "# TopAgent Memory Index\n\n- title: arch | file: notes/arch.md | status: verified\n",
    )
    .unwrap();

    fs::write(
        topagent_dir.join("USER.md"),
        "# Operator Preferences\nPrefer short functions.\n",
    )
    .unwrap();

    fs::write(temp.path().join("main.rs"), "fn main() {}").unwrap();

    store
        .capture_file(
            "main.rs",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "before mutation"),
        )
        .unwrap();

    fs::write(temp.path().join("main.rs"), "fn main() { // mutated }").unwrap();

    let report = store.restore_latest().unwrap().unwrap();
    assert!(report.restored_files.contains(&"main.rs".to_string()));

    assert!(
        topagent_dir.join("MEMORY.md").exists(),
        "MEMORY.md must survive restore"
    );
    assert!(
        topagent_dir.join("USER.md").exists(),
        "USER.md must survive restore"
    );
    assert!(
        fs::read_to_string(topagent_dir.join("MEMORY.md"))
            .unwrap()
            .contains("arch"),
        "MEMORY.md content must be preserved"
    );
    assert!(
        fs::read_to_string(topagent_dir.join("USER.md"))
            .unwrap()
            .contains("Operator Preferences"),
        "USER.md content must be preserved"
    );
}

#[test]
fn test_restore_clears_run_snapshot_itself() {
    let (temp, store) = create_temp_workspace();

    fs::write(temp.path().join("data.txt"), "before").unwrap();

    store
        .capture_file(
            "data.txt",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "test"),
        )
        .unwrap();

    assert!(
        store.latest_status().unwrap().is_some(),
        "run snapshot should exist after capture"
    );

    fs::write(temp.path().join("data.txt"), "after").unwrap();

    let report = store.restore_latest().unwrap().unwrap();
    assert!(report.restored_files.contains(&"data.txt".to_string()));

    assert!(
        store.latest_status().unwrap().is_none(),
        "run snapshot should be gone after restore"
    );
}

#[test]
fn test_trust_sensitive_paths_remain_strict_for_note_promotion() {
    let contract = BehaviorContract::default();

    let low = low_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Note, &low, false)
            .is_some(),
        "note promotion must be blocked under low trust without corroboration"
    );

    let mixed = mixed_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Note, &mixed, false)
            .is_some(),
        "note promotion must be blocked when low-trust influence is present without corroboration"
    );

    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Note, &mixed, true)
            .is_none(),
        "note promotion may proceed when low-trust is data-only and corroboration is present"
    );

    let high = high_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Note, &high, false)
            .is_none(),
        "note promotion should be allowed under high trust"
    );
}

#[test]
fn test_trust_sensitive_paths_remain_strict_for_procedure_promotion() {
    let contract = BehaviorContract::default();

    let low = low_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Procedure, &low, false)
            .is_some(),
        "procedure promotion must be blocked under low trust"
    );

    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Procedure, &low, true)
            .is_some(),
        "procedure promotion must be blocked under low trust even with corroboration"
    );
}

#[test]
fn test_trust_sensitive_paths_remain_strict_for_operator_preference() {
    let contract = BehaviorContract::default();

    let low = low_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::OperatorPreference, &low, false)
            .is_some(),
        "operator preference promotion must be blocked under low trust"
    );
}

#[test]
fn test_trust_sensitive_paths_remain_strict_for_trajectory_review() {
    let contract = BehaviorContract::default();

    let low = low_trust_context();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::TrajectoryReview, &low, false)
            .is_some(),
        "trajectory review must be blocked under low trust"
    );

    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::TrajectoryExport, &low, false)
            .is_some(),
        "trajectory export must be blocked under low trust"
    );
}

#[test]
fn test_memory_write_blocking_remains_strict_under_low_trust() {
    let contract = BehaviorContract::default();

    let low = low_trust_context();
    assert!(
        contract
            .memory_write_block_reason("save_note", &low, false)
            .is_some(),
        "save_note must be blocked under low trust without corroboration"
    );
    assert!(
        contract
            .memory_write_block_reason("manage_operator_preference", &low, false)
            .is_some(),
        "manage_operator_preference must be blocked under low trust"
    );
    assert!(
        contract
            .memory_write_block_reason("manage_operator_preference", &low, true)
            .is_some(),
        "manage_operator_preference must be blocked under low trust even with corroboration"
    );
}

#[test]
fn test_memory_write_allows_trusted_context() {
    let contract = BehaviorContract::default();
    let high = high_trust_context();
    assert!(
        contract
            .memory_write_block_reason("save_note", &high, false)
            .is_none(),
        "save_note should be allowed under high trust"
    );
}

#[test]
fn test_status_reflects_latest_capture_not_stale_data() {
    let (temp, store) = create_temp_workspace();

    fs::write(temp.path().join("v1.txt"), "version1").unwrap();
    store
        .capture_file(
            "v1.txt",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "first"),
        )
        .unwrap();

    let status = store.latest_status().unwrap().unwrap();
    assert!(status.captured_paths.contains(&"v1.txt".to_string()));
    assert!(!status.captures.is_empty());
    assert_eq!(
        status.captures.last().unwrap().source,
        RunSnapshotCaptureSource::Write
    );
    assert_eq!(status.captures.last().unwrap().reason, "first");

    fs::write(temp.path().join("v2.txt"), "version2").unwrap();
    store
        .capture_file(
            "v2.txt",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Bash, "second"),
        )
        .unwrap();

    let status = store.latest_status().unwrap().unwrap();
    assert!(status.captured_paths.contains(&"v2.txt".to_string()));
    assert!(status.captured_paths.contains(&"v1.txt".to_string()));
    assert!(!status.captures.is_empty());
    assert_eq!(
        status.captures.last().unwrap().source,
        RunSnapshotCaptureSource::Bash
    );
    assert_eq!(status.captures.last().unwrap().reason, "second");
}

#[test]
fn test_restore_then_modify_then_second_restore_works_correctly() {
    let (temp, _store) = create_temp_workspace();

    fs::write(temp.path().join("important.txt"), "original content").unwrap();

    let store1 = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
    store1
        .capture_file(
            "important.txt",
            RunSnapshotCaptureMetadata::new(RunSnapshotCaptureSource::Write, "initial state"),
        )
        .unwrap();

    fs::write(temp.path().join("important.txt"), "bad change").unwrap();

    let report1 = store1.restore_latest().unwrap().unwrap();
    assert!(report1
        .restored_files
        .contains(&"important.txt".to_string()));
    assert_eq!(
        fs::read_to_string(temp.path().join("important.txt")).unwrap(),
        "original content"
    );

    fs::write(
        temp.path().join("important.txt"),
        "after restore - good state",
    )
    .unwrap();

    let store2 = WorkspaceRunSnapshotStore::new(temp.path().to_path_buf());
    store2
        .capture_file(
            "important.txt",
            RunSnapshotCaptureMetadata::new(
                RunSnapshotCaptureSource::Edit,
                "post-restore run snapshot",
            ),
        )
        .unwrap();

    fs::write(temp.path().join("important.txt"), "second bad change").unwrap();

    let report2 = store2.restore_latest().unwrap().unwrap();
    assert!(
        report2
            .restored_files
            .contains(&"important.txt".to_string()),
        "second restore should also work"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("important.txt")).unwrap(),
        "after restore - good state",
        "second restore should go back to the last run snapshot state"
    );
}

#[test]
fn test_provenance_context_survives_merge() {
    let mut ctx1 = RunTrustContext::default();
    ctx1.add_source(SourceLabel::low(
        SourceKind::FetchedWebContent,
        InfluenceMode::MayDriveAction,
        "curl https://example.com/data",
    ));

    let mut ctx2 = RunTrustContext::default();
    ctx2.add_source(SourceLabel::trusted(
        SourceKind::OperatorDirect,
        InfluenceMode::MayDriveAction,
        "operator instruction",
    ));

    ctx1.merge(&ctx2);

    assert!(ctx1.has_low_trust_sources());
    assert!(ctx1.has_low_trust_action_influence());

    let contract = BehaviorContract::default();
    assert!(
        contract
            .durable_promotion_block_reason(DurablePromotionKind::Note, &ctx1, false)
            .is_some(),
        "merged context with low-trust must still block promotions"
    );
}

#[test]
fn test_hidden_run_snapshot_paths_are_not_captured() {
    let (temp, store) = create_temp_workspace();

    let snapshots_dir = temp.path().join(".topagent/run-snapshots");
    fs::create_dir_all(&snapshots_dir).unwrap();
    fs::write(snapshots_dir.join("manifest.json"), "{}").unwrap();

    let git_dir = temp.path().join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main").unwrap();

    fs::write(temp.path().join("real_file.txt"), "content").unwrap();

    store
        .capture_workspace(RunSnapshotCaptureMetadata::new(
            RunSnapshotCaptureSource::Write,
            "workspace scan",
        ))
        .unwrap();

    let status = store.latest_status().unwrap().unwrap();
    assert!(
        status.captured_paths.contains(&"real_file.txt".to_string()),
        "real files should be captured"
    );
    assert!(
        !status.captured_paths.iter().any(|p| p.contains(".git")),
        ".git paths should not be captured"
    );
    assert!(
        !status
            .captured_paths
            .iter()
            .any(|p| p.contains(".topagent/run-snapshots")),
        "run snapshot paths should not be captured"
    );
}

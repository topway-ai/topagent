use topagent_core::{
    BehaviorContract, DeliveryOutcome, DurablePromotionKind, InfluenceMode, RunTrustContext,
    SourceKind, SourceLabel, TaskMode, TaskResult, VerificationCommand,
};

fn low_trust_context() -> RunTrustContext {
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::low(
        SourceKind::FetchedWebContent,
        InfluenceMode::MayDriveAction,
        "curl https://example.com/install.sh",
    ));
    trust
}

#[test]
fn test_memory_write_block_reason_blocks_operator_preference_for_low_trust() {
    let contract = BehaviorContract::default();
    let trust = low_trust_context();

    let block = contract
        .memory_write_block_reason("manage_operator_preference", &trust, false)
        .expect("operator preference writes should be blocked");

    assert!(block.contains("operator preference"));
    assert!(block.contains("low-trust content"));
}

#[test]
fn test_durable_promotion_policy_respects_low_trust_and_corroboration() {
    let contract = BehaviorContract::default();
    let trust = low_trust_context();

    assert_eq!(
        contract.durable_promotion_block_reason(DurablePromotionKind::Lesson, &trust, true),
        None
    );
    assert!(contract
        .durable_promotion_block_reason(DurablePromotionKind::Lesson, &trust, false)
        .unwrap()
        .contains("trusted workspace corroboration"));
    assert!(contract
        .durable_promotion_block_reason(DurablePromotionKind::Procedure, &trust, true)
        .unwrap()
        .contains("cannot become a reusable procedure"));
}

#[test]
fn test_should_attach_proof_of_work_matches_output_policy() {
    let contract = BehaviorContract::default();

    assert!(!contract.should_attach_proof_of_work(0, 0, 0, 0));
    assert!(contract.should_attach_proof_of_work(1, 0, 0, 0));
    assert!(contract.should_attach_proof_of_work(0, 1, 0, 0));
    assert!(contract.should_attach_proof_of_work(0, 0, 1, 0));
    assert!(contract.should_attach_proof_of_work(0, 0, 0, 1));
}

#[test]
fn test_render_memory_index_template_uses_contract_policy() {
    let contract = BehaviorContract::default();
    let template = contract.render_memory_index_template();

    assert!(template.contains("# TopAgent Memory Index"));
    assert!(template.contains("Keep this file tiny"));
    assert!(template.contains("Use this file as an index only"));
    assert!(template.contains(contract.memory.index_entry_format));
    assert!(template.contains("transcripts, logs"));
}

#[test]
fn test_should_attach_code_delivery_summary_plan_and_execute_with_files() {
    let contract = BehaviorContract::default();

    assert!(contract.should_attach_code_delivery_summary(TaskMode::PlanAndExecute, 1, 0));
    assert!(contract.should_attach_code_delivery_summary(TaskMode::PlanAndExecute, 1, 1));
    assert!(!contract.should_attach_code_delivery_summary(TaskMode::PlanAndExecute, 0, 0));
    assert!(!contract.should_attach_code_delivery_summary(TaskMode::InspectOnly, 1, 0));
    assert!(!contract.should_attach_code_delivery_summary(TaskMode::VerifyOnly, 1, 0));
}

#[test]
fn test_format_verification_status_no_files() {
    let contract = BehaviorContract::default();

    assert!(contract
        .format_verification_status(TaskMode::PlanAndExecute, 0, &[])
        .is_none());
    assert!(contract
        .format_verification_status(TaskMode::InspectOnly, 1, &[])
        .is_none());
}

#[test]
fn test_format_verification_status_no_verification() {
    let contract = BehaviorContract::default();

    let status = contract
        .format_verification_status(TaskMode::PlanAndExecute, 1, &[])
        .unwrap();
    assert!(status.contains("not verified"));
}

#[test]
fn test_format_verification_status_passed() {
    let contract = BehaviorContract::default();

    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "ok".to_string(),
        exit_code: 0,
        succeeded: true,
    };
    let status = contract
        .format_verification_status(TaskMode::PlanAndExecute, 1, &[cmd])
        .unwrap();
    assert!(status.contains("passed"));
}

#[test]
fn test_format_verification_status_failed() {
    let contract = BehaviorContract::default();

    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "error".to_string(),
        exit_code: 1,
        succeeded: false,
    };
    let status = contract
        .format_verification_status(TaskMode::PlanAndExecute, 1, &[cmd])
        .unwrap();
    assert!(status.contains("failed"));
}

#[test]
fn test_delivery_outcome_enum_values() {
    assert_eq!(DeliveryOutcome::None, DeliveryOutcome::None);
    assert_eq!(DeliveryOutcome::AnalysisOnly, DeliveryOutcome::AnalysisOnly);
    assert_eq!(DeliveryOutcome::NoOp, DeliveryOutcome::NoOp);
    assert_eq!(
        DeliveryOutcome::CodeChangingVerified,
        DeliveryOutcome::CodeChangingVerified
    );
    assert_eq!(
        DeliveryOutcome::CodeChangingUnverified,
        DeliveryOutcome::CodeChangingUnverified
    );
    assert_eq!(
        DeliveryOutcome::CodeChangingFailed,
        DeliveryOutcome::CodeChangingFailed
    );
}

#[test]
fn test_format_delivery_summary_with_files_and_verification() {
    let result = TaskResult::new("Fixed the bug".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingVerified)
        .with_verification_command(VerificationCommand {
            command: "cargo test".to_string(),
            output: "test result: ok".to_string(),
            exit_code: 0,
            succeeded: true,
        });

    let summary = result.format_delivery_summary().unwrap();
    assert!(summary.contains("Delivery Summary"));
    assert!(summary.contains("Files Touched"));
    assert!(summary.contains("src/main.rs"));
    assert!(summary.contains("Verification Status"));
    assert!(summary.contains("cargo test"));
    assert!(summary.contains("✅ PASS"));
    assert!(summary.contains("Suggested Next Step"));
    assert!(summary.contains("Review changes"));
}

#[test]
fn test_format_delivery_summary_no_files() {
    let result = TaskResult::new("Analyzed the codebase".to_string())
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::AnalysisOnly)
        .with_verification_command(VerificationCommand {
            command: "cargo check".to_string(),
            output: "ok".to_string(),
            exit_code: 0,
            succeeded: true,
        });

    let summary = result.format_delivery_summary().unwrap();
    assert!(summary.contains("Analysis/verification run"));
}

#[test]
fn test_format_delivery_summary_unverified_with_reason() {
    let result = TaskResult::new("Updated config".to_string())
        .with_files_changed(vec!["config.yaml".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingUnverified)
        .with_verification_skip_reason("verification not attempted".to_string());

    let summary = result.format_delivery_summary().unwrap();
    assert!(summary.contains("Verification Status"));
    assert!(summary.contains("Not verified"));
    assert!(summary.contains("verification not attempted"));
    assert!(summary.contains("Run verification manually"));
}

#[test]
fn test_format_delivery_summary_failed_verification() {
    let result = TaskResult::new("Made changes".to_string())
        .with_files_changed(vec!["src/lib.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingFailed)
        .with_verification_command(VerificationCommand {
            command: "cargo test".to_string(),
            output: "error: test failed".to_string(),
            exit_code: 1,
            succeeded: false,
        });

    let summary = result.format_delivery_summary().unwrap();
    assert!(summary.contains("Verification Status"));
    assert!(summary.contains("cargo test"));
    assert!(summary.contains("❌ FAIL"));
    assert!(summary.contains("Fix failing verification"));
}

#[test]
fn test_format_delivery_summary_inspect_only_returns_none() {
    let result = TaskResult::new("Analysis complete".to_string())
        .with_task_mode(TaskMode::InspectOnly)
        .with_files_changed(vec![]);

    assert!(result.format_delivery_summary().is_none());
}

#[test]
fn test_task_result_verification_commands_accessible() {
    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "ok".to_string(),
        exit_code: 0,
        succeeded: true,
    };
    let result = TaskResult::new("Done".to_string()).with_verification_command(cmd);

    let vcs = result.verification_commands();
    assert_eq!(vcs.len(), 1);
    assert_eq!(vcs[0].command, "cargo test");
    assert!(result.final_verification_passed());
}

#[test]
fn test_task_result_verification_skip_reason_accessible() {
    let result = TaskResult::new("Done".to_string())
        .with_verification_skip_reason("no obvious verification command available".to_string());

    assert_eq!(
        result.verification_skip_reason(),
        Some("no obvious verification command available")
    );
}

#[test]
fn test_task_result_delivery_outcome_with_verification_passed() {
    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "ok".to_string(),
        exit_code: 0,
        succeeded: true,
    };
    let result = TaskResult::new("Done".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_verification_command(cmd)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingVerified);

    assert!(result.has_files_changed());
    assert!(result.final_verification_passed());
    assert_eq!(
        result.delivery_outcome(),
        DeliveryOutcome::CodeChangingVerified
    );
}

#[test]
fn test_task_result_delivery_outcome_with_verification_failed() {
    let cmd = VerificationCommand {
        command: "cargo test".to_string(),
        output: "FAILED".to_string(),
        exit_code: 1,
        succeeded: false,
    };
    let result = TaskResult::new("Done".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_verification_command(cmd)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingFailed);

    assert!(result.has_files_changed());
    assert!(!result.final_verification_passed());
    assert_eq!(
        result.delivery_outcome(),
        DeliveryOutcome::CodeChangingFailed
    );
}

#[test]
fn test_task_result_delivery_outcome_no_files_no_verification() {
    let result = TaskResult::new("Done".to_string()).with_delivery_outcome(DeliveryOutcome::NoOp);

    assert!(!result.has_files_changed());
    assert!(result.verification_commands().is_empty());
    assert_eq!(result.delivery_outcome(), DeliveryOutcome::NoOp);
}

#[test]
fn test_task_result_delivery_outcome_unverified_with_skip_reason() {
    let result = TaskResult::new("Done".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_delivery_outcome(DeliveryOutcome::CodeChangingUnverified)
        .with_verification_skip_reason("no obvious verification command available".to_string());

    assert!(result.has_files_changed());
    assert!(result.verification_commands().is_empty());
    assert_eq!(
        result.delivery_outcome(),
        DeliveryOutcome::CodeChangingUnverified
    );
    assert_eq!(
        result.verification_skip_reason(),
        Some("no obvious verification command available")
    );
}

#[test]
fn test_delivery_summary_shows_verification_commands_in_compact_form() {
    let result = TaskResult::new("Fixed the bug".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingVerified)
        .with_verification_command(VerificationCommand {
            command: "cargo test -p topagent-core".to_string(),
            output: "test result: ok".to_string(),
            exit_code: 0,
            succeeded: true,
        });

    let summary = result.format_delivery_summary().unwrap();
    // Should show command in compact form
    assert!(summary.contains("cargo test"));
    // Should show PASS status
    assert!(summary.contains("✅ PASS"));
    // Should show Files Touched
    assert!(summary.contains("Files Touched"));
}

#[test]
fn test_delivery_summary_shows_skip_reason_for_unverified() {
    let result = TaskResult::new("Updated config".to_string())
        .with_files_changed(vec!["config.yaml".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingUnverified)
        .with_verification_skip_reason("no obvious verification command available".to_string());

    let summary = result.format_delivery_summary().unwrap();
    // Must show that verification was NOT performed
    assert!(summary.contains("Not verified"));
    // Must show the skip reason
    assert!(summary.contains("no obvious verification command available"));
    // Must show a useful next step
    assert!(summary.contains("Run verification manually"));
}

#[test]
fn test_delivery_summary_shows_failed_verification_explicitly() {
    let result = TaskResult::new("Made changes".to_string())
        .with_files_changed(vec!["src/lib.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingFailed)
        .with_verification_command(VerificationCommand {
            command: "cargo test".to_string(),
            output: "error: test failed\n    --> src/lib.rs:10".to_string(),
            exit_code: 1,
            succeeded: false,
        });

    let summary = result.format_delivery_summary().unwrap();
    // Must explicitly show failed status
    assert!(summary.contains("❌ FAIL"));
    // Must show next step
    assert!(summary.contains("Fix failing verification"));
}

#[test]
fn test_delivery_summary_shows_remaining_risks_when_present() {
    let result = TaskResult::new("Fixed the bug".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingUnverified)
        .with_verification_skip_reason("tests require database".to_string())
        .with_unresolved_issue("Tests require database setup".to_string());

    let summary = result.format_delivery_summary().unwrap();
    // Must show remaining risks explicitly
    assert!(summary.contains("Remaining Risks"));
    assert!(summary.contains("Tests require database setup"));
}

#[test]
fn test_delivery_summary_shows_verification_command_name_for_passed() {
    let result = TaskResult::new("Build success".to_string())
        .with_files_changed(vec!["src/main.rs".to_string()])
        .with_task_mode(TaskMode::PlanAndExecute)
        .with_delivery_outcome(DeliveryOutcome::CodeChangingVerified)
        .with_verification_command(VerificationCommand {
            command: "cargo check".to_string(),
            output: "Checking topagent-core".to_string(),
            exit_code: 0,
            succeeded: true,
        });

    let summary = result.format_delivery_summary().unwrap();
    // Should show the actual command that was run
    assert!(summary.contains("cargo check"));
    // Should show passed status
    assert!(summary.contains("✅ PASS"));
}

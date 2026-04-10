use topagent_core::behavior::PreExecutionState;
use topagent_core::{BehaviorContract, ExternalToolEffect, RuntimeOptions, TaskMode};

#[test]
fn test_behavior_contract_respects_runtime_options() {
    let options = RuntimeOptions::default()
        .with_require_plan(false)
        .with_generated_tool_authoring(true)
        .with_max_messages_before_truncation(42);
    let contract = BehaviorContract::from_runtime_options(&options);

    assert!(!contract.planning.require_plan_by_default);
    assert!(contract.generated_tools.authoring_enabled);
    assert_eq!(contract.compaction.max_messages_before_truncation, 42);
    assert_eq!(contract.compaction.micro_trigger_messages, 21);
}

#[test]
fn test_classify_task_fast_path_matches_current_rules() {
    let contract = BehaviorContract::default();

    assert_eq!(
        contract.classify_task_fast_path("make a plan for the refactor"),
        Some(true)
    );
    assert_eq!(
        contract.classify_task_fast_path("refactor the entire repo"),
        Some(true)
    );
    assert_eq!(
        contract.classify_task_fast_path("read this file"),
        Some(false)
    );
    assert_eq!(
        contract.classify_task_fast_path("fix the typo in main.rs"),
        Some(false)
    );
}

#[test]
fn test_task_mode_fast_path_detects_mutation_and_defers_findings() {
    let contract = BehaviorContract::default();

    assert_eq!(
        contract.task_mode_fast_path("Make a plan and implement the feature"),
        Some(TaskMode::PlanAndExecute)
    );
    assert_eq!(
        contract
            .task_mode_fast_path("Make a plan to assess the repository and return findings only"),
        None
    );
}

#[test]
fn test_planning_block_message_only_blocks_mutation_risk_without_plan() {
    let contract = BehaviorContract::default();

    assert!(contract
        .planning_block_message("bash", Some("git status"), None, false,)
        .is_none());

    assert!(contract
        .planning_block_message("bash", Some("touch risky.txt"), None, false,)
        .unwrap()
        .contains("Planning required"));

    assert!(contract
        .planning_block_message(
            "read_only_tool",
            None,
            Some(ExternalToolEffect::ReadOnly),
            false,
        )
        .is_none());
}

#[test]
fn test_pre_execution_block_message_requires_real_execution_before_verify() {
    let contract = BehaviorContract::default();
    let state = PreExecutionState {
        planning_required_for_task: true,
        plan_exists: true,
        execution_started: false,
        task_mode: TaskMode::PlanAndExecute,
    };

    let bash_block =
        contract.pre_execution_block_message("bash", Some("cargo check --offline"), None, &state);
    assert!(bash_block
        .unwrap()
        .contains("Execute at least one plan step"));

    let external_block = contract.pre_execution_block_message(
        "verify_tool",
        None,
        Some(ExternalToolEffect::VerificationOnly),
        &state,
    );
    assert!(external_block.unwrap().contains("verification tools"));

    let executed_state = PreExecutionState {
        execution_started: true,
        ..state
    };
    assert!(contract
        .pre_execution_block_message("bash", Some("cargo check --offline"), None, &executed_state,)
        .is_none());
}

#[test]
fn test_should_escalate_to_planning_after_unplanned_multi_file_mutation() {
    let contract = BehaviorContract::default();

    assert!(contract.should_escalate_to_planning(false, false, false, 3));
    assert!(!contract.should_escalate_to_planning(true, false, false, 3));
    assert!(!contract.should_escalate_to_planning(false, true, false, 3));
    assert!(!contract.should_escalate_to_planning(false, false, true, 3));
    assert!(!contract.should_escalate_to_planning(false, false, false, 2));
}

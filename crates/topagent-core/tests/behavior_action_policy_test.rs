use topagent_core::{BashCommandClass, BehaviorContract};

#[test]
fn test_classify_bash_command_routes_expected_classes() {
    let contract = BehaviorContract::default();

    assert_eq!(
        contract.classify_bash_command("git status"),
        BashCommandClass::ResearchSafe
    );
    assert_eq!(
        contract.classify_bash_command("cargo test --lib"),
        BashCommandClass::Verification
    );
    assert_eq!(
        contract.classify_bash_command("echo hi > file.txt"),
        BashCommandClass::MutationRisk
    );
    assert_eq!(
        contract.classify_bash_command("find . -type f 2>/dev/null | head -20"),
        BashCommandClass::ResearchSafe
    );
    assert_eq!(
        contract.classify_bash_command("cd /tmp/topagent && cargo test 2>&1 | tail -20"),
        BashCommandClass::Verification
    );
    assert_eq!(
        contract.classify_bash_command("find . -delete"),
        BashCommandClass::MutationRisk
    );
}

#[test]
fn test_tool_and_mutation_membership_remains_correct() {
    let contract = BehaviorContract::default();

    assert!(contract.is_planning_tool("update_plan"));
    assert!(contract.is_memory_write_tool("manage_operator_preference"));
    assert!(contract.is_mutation_tool("write"));
    assert!(contract.is_generated_tool_authoring_tool("create_tool"));
    assert!(contract.mutates_generated_tool_surface("delete_generated_tool"));
    assert!(contract.is_verification_command("cargo check --offline"));
    assert!(!contract.is_verification_command("git status"));
}

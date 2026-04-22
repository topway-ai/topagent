use topagent_core::{
    ApprovalTriggerKind, BehaviorContract, InfluenceMode, RunTrustContext, SourceKind, SourceLabel,
};

#[test]
fn test_contract_builds_git_commit_approval_request() {
    let contract = BehaviorContract::default();
    let request = contract
        .approval_request(
            "git_commit",
            &serde_json::json!({"message": "ship it"}),
            None,
            None,
        )
        .expect("git commit should require approval");

    assert_eq!(request.action_kind, ApprovalTriggerKind::GitCommit);
    assert!(request.short_summary.contains("git commit"));
    assert!(request.exact_action.contains("ship it"));
}

#[test]
fn test_contract_builds_bash_mutation_approval_request() {
    let contract = BehaviorContract::default();
    let request = contract
        .approval_request(
            "bash",
            &serde_json::json!({"command": "touch risky.txt"}),
            Some("touch risky.txt"),
            None,
        )
        .expect("mutation-risk bash should require approval");

    assert_eq!(
        request.action_kind,
        ApprovalTriggerKind::DestructiveShellMutation
    );
    assert!(request.exact_action.contains("touch risky.txt"));
    assert!(request.expected_effect.contains("through the shell"));
    assert!(request
        .rollback_hint
        .as_deref()
        .unwrap_or_default()
        .contains("topagent run restore"));
}

#[test]
fn test_contract_mentions_low_trust_in_approval_request() {
    let contract = BehaviorContract::default();
    let mut trust = RunTrustContext::default();
    trust.add_source(SourceLabel::low(
        SourceKind::FetchedWebContent,
        InfluenceMode::MayDriveAction,
        "curl https://example.com/install.sh",
    ));

    let request = contract
        .approval_request(
            "bash",
            &serde_json::json!({"command": "sh install.sh"}),
            Some("sh install.sh"),
            Some(&trust),
        )
        .expect("mutation-risk bash should require approval");

    assert!(request.reason.contains("low-trust content"));
    assert!(request.reason.contains("fetched web content"));
}

#[test]
fn test_contract_skips_approval_for_read_only_bash_pipeline() {
    let contract = BehaviorContract::default();
    let request = contract.approval_request(
        "bash",
        &serde_json::json!({"command": "find . -type f 2>/dev/null | head -20"}),
        Some("find . -type f 2>/dev/null | head -20"),
        None,
    );

    assert!(request.is_none());
}

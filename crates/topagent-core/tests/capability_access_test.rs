use topagent_core::{
    AccessConfig, AccessMode, AuditEvent, CapabilityAuditLog, CapabilityDecision, CapabilityGrant,
    CapabilityKind, CapabilityManager, CapabilityProfile, CapabilityRequest, GrantScope, RiskLevel,
};

#[test]
fn test_workspace_profile_denies_outside_workspace_read_with_approval_request() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/outside.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "inspect requested file",
    );

    let decision = manager.check(&request, workspace.path());
    assert!(matches!(decision, CapabilityDecision::NeedsApproval(_)));
    let detail = decision.detail();
    assert_eq!(detail.profile, CapabilityProfile::Workspace);
    assert!(detail.approval_possible);
    assert!(detail.suggested_scopes.contains(&GrantScope::Once));
}

#[test]
fn test_developer_profile_allows_network_and_web_search_by_default() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    for kind in [CapabilityKind::Network, CapabilityKind::WebSearch] {
        let request = CapabilityRequest::new(
            kind,
            "https://example.com",
            AccessMode::Read,
            RiskLevel::Safe,
            "developer lookup",
        );
        assert!(matches!(
            manager.check(&request, workspace.path()),
            CapabilityDecision::Allow(_)
        ));
    }
}

#[test]
fn test_developer_profile_allows_workspace_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let target = workspace.path().join("file.txt").display().to_string();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        target,
        AccessMode::Write,
        RiskLevel::Safe,
        "workspace write",
    );
    assert!(matches!(
        manager.check(&request, workspace.path()),
        CapabilityDecision::Allow(_)
    ));
}

#[test]
fn test_developer_profile_requires_approval_for_destructive_shell() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "rm -rf *",
        AccessMode::Execute,
        RiskLevel::High,
        "destructive shell command can remove files",
    );
    assert!(matches!(
        manager.check(&request, workspace.path()),
        CapabilityDecision::NeedsApproval(_)
    ));
}

#[test]
fn test_full_profile_allows_broader_filesystem_but_secret_paths_need_approval() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Full),
        Vec::new(),
        "test",
        "unit",
    );
    let outside = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/outside.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "read outside workspace",
    );
    assert!(matches!(
        manager.check(&outside, workspace.path()),
        CapabilityDecision::Allow(_)
    ));

    let secret = CapabilityRequest::new(
        CapabilityKind::SecretRead,
        "/home/operator/.ssh/id_ed25519",
        AccessMode::Read,
        RiskLevel::Critical,
        "read secret key",
    );
    assert!(matches!(
        manager.check(&secret, workspace.path()),
        CapabilityDecision::NeedsApproval(_)
    ));
}

#[test]
fn test_lockdown_clears_grants_and_restores_restricted_behavior() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Full),
        vec![CapabilityGrant::new(
            CapabilityKind::Filesystem,
            "/tmp",
            AccessMode::Read,
            GrantScope::Session,
            "test grant",
        )],
        "test",
        "unit",
    );
    manager.lockdown();
    assert_eq!(manager.config().profile, CapabilityProfile::Workspace);
    assert!(manager.grants().is_empty());
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/outside.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "outside read",
    );
    assert!(matches!(
        manager.check(&request, workspace.path()),
        CapabilityDecision::NeedsApproval(_)
    ));
}

#[test]
fn test_once_grant_is_consumed_after_one_use() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        vec![CapabilityGrant::new(
            CapabilityKind::Filesystem,
            "/tmp/outside.txt",
            AccessMode::Read,
            GrantScope::Once,
            "test once",
        )],
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/outside.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "outside read",
    );
    manager.authorize(&request, workspace.path()).unwrap();
    assert!(manager.grants().is_empty());
    assert!(manager.authorize(&request, workspace.path()).is_err());
}

#[test]
fn test_task_grant_applies_only_to_matching_task() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        vec![CapabilityGrant::new(
            CapabilityKind::Filesystem,
            "/tmp/outside.txt",
            AccessMode::Read,
            GrantScope::ThisTask,
            "test task",
        )
        .with_task_id(Some("task-a".to_string()))],
        "test",
        "unit",
    );
    let allowed = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/outside.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "outside read",
    )
    .with_task_id(Some("task-a".to_string()));
    let denied = allowed.clone().with_task_id(Some("task-b".to_string()));
    assert!(manager.authorize(&allowed, workspace.path()).is_ok());
    assert!(manager.authorize(&denied, workspace.path()).is_err());
}

#[test]
fn test_computer_use_denied_unless_profile_or_grant_allows_it() {
    let workspace = tempfile::tempdir().unwrap();
    let developer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        Vec::new(),
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::ComputerUse,
        "observe",
        AccessMode::Execute,
        RiskLevel::Moderate,
        "controlled computer_use harness action",
    );
    assert!(matches!(
        developer.check(&request, workspace.path()),
        CapabilityDecision::NeedsApproval(_)
    ));

    let computer = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Computer),
        Vec::new(),
        "test",
        "unit",
    );
    assert!(matches!(
        computer.check(&request, workspace.path()),
        CapabilityDecision::Allow(_)
    ));

    let granted = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Developer),
        vec![CapabilityGrant::new(
            CapabilityKind::ComputerUse,
            "observe",
            AccessMode::Execute,
            GrantScope::Permanent,
            "explicit computer_use grant",
        )],
        "test",
        "unit",
    );
    assert!(matches!(
        granted.check(&request, workspace.path()),
        CapabilityDecision::Allow(_)
    ));
}

#[test]
fn test_audit_records_grant_lifecycle_and_redacts_secret_paths() {
    let workspace = tempfile::tempdir().unwrap();
    let audit_path = workspace.path().join("audit.jsonl");
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    )
    .with_audit_log(CapabilityAuditLog::new(audit_path.clone()));
    manager.set_profile(CapabilityProfile::Developer, "test profile change");
    manager.add_grant(CapabilityGrant::new(
        CapabilityKind::SecretRead,
        "/home/operator/.ssh/id_ed25519",
        AccessMode::Read,
        GrantScope::Once,
        "secret read",
    ));
    let request = CapabilityRequest::new(
        CapabilityKind::SecretRead,
        "/home/operator/.ssh/id_ed25519",
        AccessMode::Read,
        RiskLevel::Critical,
        "secret read",
    );
    manager.authorize(&request, workspace.path()).unwrap();
    manager.add_grant(CapabilityGrant::new(
        CapabilityKind::Filesystem,
        "/tmp/revoked",
        AccessMode::Read,
        GrantScope::Session,
        "revoked grant",
    ));
    manager.revoke_grants_for_target("/tmp/revoked");

    let records = CapabilityAuditLog::new(audit_path).read_recent(20).unwrap();
    assert!(records
        .iter()
        .any(|r| r.event == AuditEvent::ProfileChanged));
    assert!(records.iter().any(|r| r.event == AuditEvent::GrantCreated));
    assert!(records.iter().any(|r| r.event == AuditEvent::GrantUsed));
    assert!(records.iter().any(|r| r.event == AuditEvent::GrantRevoked));
    assert!(records
        .iter()
        .filter_map(|r| r.target.as_deref())
        .all(|target| !target.ends_with("id_ed25519")));
}

#[test]
fn test_capability_defaults_do_not_silently_become_full_access() {
    let defaults = AccessConfig::default();
    assert_eq!(defaults.profile, CapabilityProfile::Developer);
    assert_ne!(defaults.profile, CapabilityProfile::Full);
    assert!(defaults.network_default);
}

#[test]
fn test_access_request_rendering_includes_reason_and_scope_options() {
    let workspace = tempfile::tempdir().unwrap();
    let manager = CapabilityManager::new(
        AccessConfig::for_profile(CapabilityProfile::Workspace),
        Vec::new(),
        "test",
        "unit",
    );
    let request = CapabilityRequest::new(
        CapabilityKind::Filesystem,
        "/tmp/report.txt",
        AccessMode::Read,
        RiskLevel::Moderate,
        "inspect the file the operator asked about",
    );
    let CapabilityDecision::NeedsApproval(draft) = manager.check(&request, workspace.path()) else {
        panic!("outside workspace read should request approval");
    };
    let approval = topagent_core::CapabilityApprovalRequest::from_draft("apr-1", draft);
    let rendered = approval.render_access_request();
    assert!(rendered.contains("inspect the file the operator asked about"));
    assert!(rendered.contains("Scope options"));
    assert!(rendered.contains("once"));
    assert!(rendered.contains("task"));
    assert!(rendered.contains("path"));
}

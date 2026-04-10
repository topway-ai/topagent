use topagent_core::{BehaviorContract, RuntimeOptions};

#[test]
fn test_truncation_notice_mentions_preserved_sections() {
    let contract = BehaviorContract::default();
    let notice = contract.build_truncation_notice(9);

    assert!(notice.contains("Previous 9 messages truncated"));
    assert!(notice.contains("behavior contract"));
    assert!(notice.contains("proof-of-work anchors"));
}

#[test]
fn test_compaction_thresholds_and_counts_follow_runtime_options() {
    let contract = BehaviorContract::from_runtime_options(
        &RuntimeOptions::default().with_max_messages_before_truncation(40),
    );

    assert_eq!(contract.keep_recent_message_count(), 20);
    assert_eq!(contract.full_rebuild_recent_message_count(), 8);
    assert!(contract.should_micro_compact(20));
    assert!(!contract.should_micro_compact(19));
    assert!(contract.should_auto_compact(40));
    assert!(!contract.should_auto_compact(39));
}

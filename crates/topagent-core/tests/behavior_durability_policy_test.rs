use topagent_core::{
    BehaviorContract, DurablePromotionKind, InfluenceMode, RunTrustContext, SourceKind, SourceLabel,
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

use crate::plan;

pub(super) struct PlanningGate {
    gate_active: bool,
    required_for_task: bool,
    pub(super) task_mode: plan::TaskMode,
    escalated: bool,
    block_count: usize,
}

impl PlanningGate {
    pub(super) fn new() -> Self {
        Self {
            gate_active: false,
            required_for_task: false,
            task_mode: plan::TaskMode::PlanAndExecute,
            escalated: false,
            block_count: 0,
        }
    }

    pub(super) fn activate(&mut self, required: bool, task_mode: plan::TaskMode) {
        self.gate_active = required;
        self.required_for_task = required;
        self.task_mode = task_mode;
        self.escalated = false;
        self.block_count = 0;
    }

    pub(super) fn deactivate(&mut self) {
        self.gate_active = false;
        self.block_count = 0;
    }

    pub(super) fn escalate(&mut self) {
        self.gate_active = true;
        self.required_for_task = true;
        self.escalated = true;
    }

    pub(super) fn note_block(&mut self) {
        self.block_count += 1;
    }

    pub(super) fn reset_block_count(&mut self) {
        self.block_count = 0;
    }

    pub(super) fn is_active(&self) -> bool {
        self.gate_active
    }

    pub(super) fn is_required_for_task(&self) -> bool {
        self.required_for_task
    }

    pub(super) fn is_escalated(&self) -> bool {
        self.escalated
    }

    pub(super) fn block_count(&self) -> usize {
        self.block_count
    }

    pub(super) fn task_mode(&self) -> plan::TaskMode {
        self.task_mode
    }

    pub(super) fn is_blocked(&self, plan_exists: bool) -> bool {
        self.gate_active && !plan_exists && self.block_count > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_gate_is_inactive() {
        let gate = PlanningGate::new();
        assert!(!gate.is_active());
        assert!(!gate.is_required_for_task());
        assert!(!gate.is_escalated());
        assert_eq!(gate.block_count(), 0);
        assert_eq!(gate.task_mode(), plan::TaskMode::PlanAndExecute);
        assert!(!gate.is_blocked(false));
        assert!(!gate.is_blocked(true));
    }

    #[test]
    fn test_activate_with_required_true_sets_active_state() {
        let mut gate = PlanningGate::new();
        gate.activate(true, plan::TaskMode::PlanAndExecute);
        assert!(gate.is_active());
        assert!(gate.is_required_for_task());
        assert!(!gate.is_escalated());
        assert_eq!(gate.block_count(), 0);
        assert_eq!(gate.task_mode(), plan::TaskMode::PlanAndExecute);
    }

    #[test]
    fn test_activate_with_required_false_keeps_inactive() {
        let mut gate = PlanningGate::new();
        gate.activate(false, plan::TaskMode::InspectOnly);
        assert!(!gate.is_active());
        assert!(!gate.is_required_for_task());
        assert_eq!(gate.task_mode(), plan::TaskMode::InspectOnly);
    }

    #[test]
    fn test_deactivate_clears_gate_and_block_count() {
        let mut gate = PlanningGate::new();
        gate.activate(true, plan::TaskMode::PlanAndExecute);
        gate.note_block();
        gate.note_block();
        assert_eq!(gate.block_count(), 2);

        gate.deactivate();
        assert!(!gate.is_active());
        assert_eq!(gate.block_count(), 0);
    }

    #[test]
    fn test_deactivate_preserves_required_for_task() {
        let mut gate = PlanningGate::new();
        gate.activate(true, plan::TaskMode::PlanAndExecute);
        assert!(gate.is_required_for_task());

        gate.deactivate();
        assert!(!gate.is_active());
        assert!(gate.is_required_for_task());
    }

    #[test]
    fn test_escalate_activates_gate_and_sets_escalated() {
        let mut gate = PlanningGate::new();
        assert!(!gate.is_active());
        assert!(!gate.is_escalated());

        gate.escalate();
        assert!(gate.is_active());
        assert!(gate.is_required_for_task());
        assert!(gate.is_escalated());
    }

    #[test]
    fn test_escalate_from_inactive_state() {
        let mut gate = PlanningGate::new();
        gate.activate(false, plan::TaskMode::PlanAndExecute);
        assert!(!gate.is_active());

        gate.escalate();
        assert!(gate.is_active());
        assert!(gate.is_required_for_task());
        assert!(gate.is_escalated());
    }

    #[test]
    fn test_note_block_increments_count() {
        let mut gate = PlanningGate::new();
        assert_eq!(gate.block_count(), 0);

        gate.note_block();
        assert_eq!(gate.block_count(), 1);

        gate.note_block();
        assert_eq!(gate.block_count(), 2);
    }

    #[test]
    fn test_reset_block_count_zeros_count() {
        let mut gate = PlanningGate::new();
        gate.note_block();
        gate.note_block();
        gate.note_block();
        assert_eq!(gate.block_count(), 3);

        gate.reset_block_count();
        assert_eq!(gate.block_count(), 0);
    }

    #[test]
    fn test_is_blocked_requires_active_gate_no_plan_and_positive_blocks() {
        let mut gate = PlanningGate::new();

        assert!(
            !gate.is_blocked(false),
            "inactive gate should not be blocked"
        );
        assert!(
            !gate.is_blocked(true),
            "inactive gate should not be blocked"
        );

        gate.activate(true, plan::TaskMode::PlanAndExecute);
        assert!(
            !gate.is_blocked(false),
            "active gate with zero blocks should not be blocked"
        );
        assert!(
            !gate.is_blocked(true),
            "active gate with plan should not be blocked"
        );

        gate.note_block();
        assert!(
            gate.is_blocked(false),
            "active gate with blocks and no plan should be blocked"
        );
        assert!(
            !gate.is_blocked(true),
            "active gate with blocks but plan exists should not be blocked"
        );
    }

    #[test]
    fn test_activate_resets_escalation_and_blocks() {
        let mut gate = PlanningGate::new();
        gate.escalate();
        gate.note_block();
        gate.note_block();
        assert!(gate.is_escalated());
        assert_eq!(gate.block_count(), 2);

        gate.activate(true, plan::TaskMode::PlanAndExecute);
        assert!(!gate.is_escalated());
        assert_eq!(gate.block_count(), 0);
    }

    #[test]
    fn test_task_mode_varies_with_classification() {
        let mut gate = PlanningGate::new();
        gate.activate(true, plan::TaskMode::InspectOnly);
        assert_eq!(gate.task_mode(), plan::TaskMode::InspectOnly);

        gate.activate(true, plan::TaskMode::VerifyOnly);
        assert_eq!(gate.task_mode(), plan::TaskMode::VerifyOnly);

        gate.activate(false, plan::TaskMode::PlanAndExecute);
        assert_eq!(gate.task_mode(), plan::TaskMode::PlanAndExecute);
    }
}

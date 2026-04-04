# Review Rules

Read this before any meaningful code change.

TopAgent is Telegram-first, CLI-backed, local-first, workspace-scoped, operator-centric, and single-agent by default. Keep behavior explicit in code. Keep approval, compaction, and durable memory disciplined. Do not make the hot path heavier unless the gain is clear and immediate.

## Preflight Review

Before editing code, produce a short review with:

- files or modules likely to change
- hot-path impact
- boundary risks
- session-vs-durable-state risks
- whether behavior will be enforced in code or only described
- whether the change adds unnecessary persistence, abstraction, or transport coupling
- simplicity score out of 10

If the score is under 7, narrow the plan before editing.

## Rules

Check the proposed change against these rules:

1. Hot-path weight: avoid adding repeated prompt bulk, extra runtime branches, or always-loaded artifacts without a clear payoff.
2. Boundary integrity: keep policy, runtime, memory, transport, and tool logic in their own layers. Do not blur CLI or Telegram concerns into core runtime.
3. Policy honesty: if behavior matters, enforce it in code. Do not hide real policy inside prompt prose or comments.
4. Session vs durable state: do not leak live run state, blockers, approvals, or transient user wishes into durable memory.
5. Memory quality: durable memory must stay curated, bounded, and worth reloading. Prefer facts with future operator value over noise.
6. Compaction correctness: do not make objective, plan, blockers, pending approvals, active files, proof-of-work anchors, or memory briefing easy to lose.
7. Approval clarity: approval-required actions must stay explicit and non-bypassable. Do not add side doors around approval checks.
8. Tool surface discipline: do not add tools that duplicate existing tools, widen scope casually, or push product behavior into tool sprawl.
9. Transport separation: Telegram and CLI rendering should stay transport-specific; shared policy belongs in the contract or shared runtime seams.
10. Restart-persistence necessity: do not add restart durability unless correctness actually requires it.
11. Canonical artifact ownership: important facts should have one obvious owner. Prefer plan state, mailbox state, durable memory, and contract artifacts over transcript repetition.
12. Simplification vs exceptions: prefer removing branches, prose, and duplicated state. Reject changes that turn TopAgent into a pile of accumulating exceptions.

## Post-change Review

After implementation, produce a short review with:

- what improved
- what got riskier
- final simplicity score out of 10
- whether the change made TopAgent more like a stable kernel or more like a pile of accumulating exceptions

If the result is not clearly simpler or more explicit, revise before stopping.

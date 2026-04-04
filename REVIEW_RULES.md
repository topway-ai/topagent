# Review Rules

Read this before any meaningful code change.

TopAgent is Telegram-first, CLI-backed, local-first, workspace-scoped, operator-centric, and single-agent by default. Keep behavior explicit in code. Keep approval, compaction, and durable memory disciplined. Do not make the hot path heavier unless the gain is clear and immediate.

These rules protect the current TopAgent kernel. They are not a veto against feature work. The question is not "does this add complexity?" The question is "is this complexity earned now, with clear ownership and payoff?"

## Preflight Review

Before editing code, produce a short review with:

- files or modules likely to change
- hot-path impact
- boundary risks
- session-vs-durable-state risks
- whether behavior will be enforced in code or only described
- whether the change adds persistence, abstraction, tool surface, transport coupling, or new runtime state
- simplicity score out of 10
- if complexity is being added:
  - why existing structure is insufficient
  - why the new complexity is earned now
  - what simpler options were considered and rejected
- if the work is exploratory:
  - mark it explicitly as a spike
  - what question it is trying to answer
  - what success or failure looks like
  - how it will be contained
  - whether it is intended to ship or only inform a later clean implementation
- if the change could shift the product boundary:
  - does it fit the current TopAgent boundary
  - if not, is the product actually changing
  - should these rules be updated first or alongside the change

If the score is under 7, do not treat that as an automatic veto. Treat it as a warning signal: narrow the plan, justify the added cost more clearly, or explicitly contain the work as a spike before editing.

## Complexity Test

- Earned complexity: required by current product needs, clearly owned, and paid for by immediate correctness, operator trust, or capability.
- Accidental complexity: added because the implementation drifted, duplicated a path, mixed layers, or kept stale constraints alive.
- Speculative complexity: added "just in case," for imagined future reuse, or before the product boundary actually needs it.

Accept earned complexity. Remove accidental complexity. Reject speculative complexity unless the work is an explicitly labeled spike.

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
13. Product-shift honesty: if a feature does not fit the current TopAgent boundary, say so plainly. Either keep it out, or treat it as a real product shift and update these rules first or alongside the change.

## Acceptable Complexity

- adding persistence because correctness or operator trust requires it now
- adding a new tool because the existing tool surface cannot express the needed capability
- splitting a hotspot into smaller units to make the runtime easier to reason about
- adding explicit policy because the runtime surface genuinely expanded
- expanding restart durability only when correctness or operator trust clearly requires it
- shipping a narrow spike only when it is labeled, contained, and easy to replace or remove

## Unacceptable Complexity

- speculative abstractions
- mixed boundaries between core runtime, transport, policy, and memory
- hot-path bloat without clear payoff
- prompt-only enforcement for behavior that should be enforced in code
- accidental persistence of session state
- duplicate execution paths or duplicate ownership of the same fact
- features with unclear owner, unclear payoff, or unclear exit path

## Spike Rule

Exploratory work is allowed only when it is explicitly labeled as a spike, narrow in scope, isolated from permanent architecture where possible, honest about what it is trying to learn, and easy to remove or replace.

## Post-change Review

After implementation, produce a short review with:

- what improved
- what got riskier
- final simplicity score out of 10
- whether the added complexity was actually justified in the final implementation
- whether the change made TopAgent more like a stable kernel or more like a pile of accumulating exceptions
- whether these rules still fit the product boundary after the change

If the result is not clearly simpler, more explicit, or more honestly justified, revise before stopping.

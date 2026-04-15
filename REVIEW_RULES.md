# Review Rules

Read this before any meaningful code change.

TopAgent is Telegram-first, CLI-backed, local-first, workspace-scoped, operator-centric, and single-agent by default.
Keep behavior explicit in code.
Keep approval, compaction, and durable memory disciplined.
Do not make the hot path heavier unless the gain is clear and immediate.

These rules protect the current TopAgent kernel.
They are not a veto against feature work.

The question is not “does this add complexity?”
The question is “is this complexity earned now, with clear ownership and payoff?”

## Authority

- `AGENTS.md` is the entry gate.
- `REVIEW_RULES.md` is the authoritative review policy.
- If the two files diverge, update them together or treat this file as authoritative.

## Glossary

- **Meaningful code change**: any non-trivial change that can affect runtime behavior, persistence, configuration, policy, prompt assembly, retrieval, tool surface, transport behavior, operator-visible behavior, or tests relied on for correctness.
- **Hot path**: ordinary one-shot execution, ordinary Telegram handling, prompt assembly, bounded retrieval, preflight gating, and tool execution for a normal task.
- **Durable artifact**: any stored record expected to survive across sessions and influence future work, including `USER.md`, `MEMORY.md`, lessons, procedures, trajectories, checkpoints, and transcript stores.
- **Session state**: live run state, blockers, approvals, transient user wishes, active file state, and in-progress objective state.
- **Spike**: exploratory work that is intentionally non-final and explicitly contained.

## Preflight Review

Use these exact headings:

- Scope
- Likely files/modules to change
- Hot-path impact
- Repeated-task cost impact: faster / unchanged / slower
- Scaling risk
- Hard cap or invariant
- Boundary risks
- Session-vs-durable-state risks
- Enforcement in code vs prose
- Added persistence / abstraction / tool surface / transport coupling / runtime state
- Simplicity score (0-10)
- Why existing structure is insufficient
- Simpler options rejected
- Spike status (if applicable)
- Product-boundary fit

Keep each field to 1–3 sentences unless more detail is necessary.

## Simplicity Rubric

- **9–10**: simplifies ownership or removes complexity
- **7–8**: narrow earned complexity with clear payoff and strong caps
- **4–6**: mixed tradeoff; requires stronger containment or clearer justification
- **0–3**: speculative, weakly owned, or boundary-distorting

A score under 7 is not an automatic veto.
It is a warning signal: narrow the plan, justify the added cost more clearly, or contain the work as a spike.

## Complexity Test

- **Earned complexity**: required by current product needs, clearly owned, and paid for by immediate correctness, operator trust, or capability.
- **Accidental complexity**: added because the implementation drifted, duplicated a path, mixed layers, or kept stale constraints alive.
- **Speculative complexity**: added “just in case,” for imagined future reuse, or before the product boundary actually needs it.

Accept earned complexity.
Remove accidental complexity.
Reject speculative complexity unless the work is an explicitly labeled spike.

## Performance Guardrails

TopAgent may learn more over time, but repeated tasks must not get slower just because durable artifacts accumulated.

- Hot-path cost must stay bounded as `USER.md`, `MEMORY.md`, lessons, procedures, trajectories, checkpoints, and transcripts grow.
- Durable artifact growth must not imply prompt growth.
- Retrieval must stay capped and relevance-filtered.
- Repeated tasks should get faster or more predictable from procedures and memory, not slower from extra ceremony or extra always-on reasoning.
- Security, provenance, and trust layers must use narrow boundary checks, summaries, and caps.
- Approval friction must stay risk-triggered.
- Do not add a second planner, second policy engine, second retrieval engine, or second always-running reasoning layer.
- Every change must say how repeat-task latency is preserved or improved, what stays capped, and why durable learning does not make the hot path grow linearly.

## Documentation and Test Review Gate

Every meaningful change must explicitly review whether related documentation and related tests need to be added, updated, removed, or left unchanged.

This review is mandatory even when the answer is “no change needed.”
If no docs or tests change, say why.

This is a review gate, not an optional reminder.

## Test Requirement

Any change that affects runtime behavior, persistence, retrieval, hooks, approvals, promotion, transport semantics, the operational control plane, or correctness-critical outputs must add or update tests.

If a meaningful change ships without tests, it must be explicitly labeled as a spike and must explain why the test gap is temporary.

Do not “fix” broken tests by weakening their coverage unless the old coverage was wrong and you explain why the narrower coverage is more truthful.

When behavior is removed, decide whether the related tests should be deleted or rewritten. Do not leave dead tests or stale assertions behind.

## Documentation Sync

If a change alters operator-facing commands, lifecycle behavior, architecture ownership, product boundary, setup/uninstall flows, runtime guarantees, or operator expectations, update the relevant documentation in the same change unless the work is an explicitly labeled spike.

When behavior is removed, decide whether the related documentation should be deleted or rewritten. Do not leave stale docs that describe no-longer-true behavior.

## Rules

### 1. Hot-path weight

Do not add default-path work that scales with durable artifact count, transcript length, or optional subsystem complexity.

If a feature is useful but expensive, move it behind explicit invocation, bounded retrieval, or offline review flow.

### 2. Boundary integrity

Keep the product boundary honest.

Do not quietly turn TopAgent into a broader platform than it currently is.

If the change materially broadens the product boundary, update the rules and docs in the same change or treat the work as a spike.

### 3. Policy honesty

Critical behavior should be enforced in code or typed state, not only suggested in prompt prose.

Do not rely on model obedience for invariants that affect correctness, safety, trust, or persistence.

### 4. Session vs durable state

Do not blur live session state with durable memory.

Transient blockers, approvals, in-progress plan state, and chat-local wishes are not long-term knowledge unless explicitly promoted under the existing rules.

### 5. Memory quality

Keep `USER.md` and `MEMORY.md` small, curated, and purpose-specific.

Do not let them become transcript dumps, temporary task logs, or vague note piles.

Procedures, trajectories, lessons, and transcripts already exist for other memory roles.

### 6. Compaction correctness

Compaction must not discard facts required for correctness, approvals, proof-of-work, or truthful operator reporting.

If compaction changes what is retained, explain what survives and why.

### 7. Approval clarity

Approvals must remain explicit, comprehensible, and risk-triggered.

Do not widen approval friction into a universal tax, and do not create side doors that bypass approval-bearing actions.

### 8. Tool surface discipline

Do not add tool surface area lightly.

Every new tool or generated-tool behavior must have clear ownership, clear invocation semantics, and bounded runtime cost.

Optional tool-authoring or maintenance complexity must not bloat ordinary runs.

### 9. Transport separation

Keep transport/rendering concerns separate from runtime policy and runtime state.

Telegram, CLI, and service management are surfaces over the same kernel, not separate products with drifting semantics.

### 10. Restart-persistence necessity

Do not persist more state just because it is convenient.

Persist only what must survive restarts for correctness, operator trust, or intentional long-term reuse.

### 11. Canonical artifact ownership

Each important fact should have one obvious durable owner.

Do not create multiple truth sources for the same thing, especially around model config, workspace memory, procedures, trajectories, generated-tool state, or operator-visible status.

### 12. Bounded retrieval

Retrieval must remain capped, relevance-filtered, and explainable.

Do not let more files on disk imply more prompt context by default.

Trajectories remain export/review artifacts, not prompt-memory.

### 13. Simplification vs exceptions

Prefer fewer owners, fewer branches, and fewer special cases.

If a change introduces another exception path, explain why the existing path could not be extended safely.

Reject decorative refactors that merely move complexity around.

### 14. Product-shift honesty

If the change makes TopAgent more like a stable kernel, say how.

If it makes TopAgent more like a pile of accumulating exceptions, stop and narrow it.

Do not disguise product drift as refactoring.

### 15. Docs and tests are part of the change

A code change is not fully reviewed until the author has explicitly checked whether related docs and tests need to change.

“Code only” is acceptable only when that answer is explicitly true and stated.

If behavior changed and docs/tests were left stale, the change is incomplete.

## Acceptable Complexity

Accept complexity that is:

- required by the current product boundary
- clearly owned
- bounded on the hot path
- backed by tests or explicit temporary spike containment
- improving correctness, operator trust, or real capability immediately

Good examples:

- a narrow new gate that closes a real correctness hole
- a bounded retrieval improvement that reduces prompt bloat
- a small control-plane split that makes config truth clearer
- a testable ownership split that reduces cross-seam ambiguity
- a narrow doc/test update that keeps behavior, operator guidance, and assertions aligned

## Unacceptable Complexity

Reject complexity that:

- exists mainly for imagined future reuse
- adds a second truth source
- broadens the default hot path without strong immediate payoff
- creates a second planner, second policy engine, second retrieval engine, or another always-running reasoning layer
- turns optional authoring or maintenance work into default runtime cost
- weakens trust boundaries, approval semantics, or durable-memory discipline
- mixes runtime policy, transport behavior, persistence, and rendering into the same owner
- changes behavior without checking whether related docs and tests must change

## Spike Rule

Exploratory work is allowed only when it is explicitly labeled as a spike, narrow in scope, isolated from permanent architecture where possible, honest about what it is trying to learn, and easy to remove or replace.

A spike must name:

- the question
- the containment boundary
- the success/failure criteria
- the cleanup or replacement plan

## Exception Handling

If a change intentionally violates one of these rules, name the rule explicitly, explain why the exception is necessary now, and contain the exception so it does not silently become the new default.

## Post-change Review

Use these exact headings:

- What improved
- What got riskier
- Final simplicity score
- Actual repeated-task cost impact: faster / unchanged / slower
- Scaling with durable artifact count
- Hard cap or regression test
- Was the added complexity actually justified?
- Stable kernel or accumulating exceptions?
- Do these rules still fit the product boundary?

Also state explicitly:

- whether related documentation was added, updated, removed, or intentionally left unchanged, and why
- whether related tests were added, updated, removed, or intentionally left unchanged, and why

If the result is not clearly simpler, more explicit, more honestly justified, or still leaves stale docs/tests behind, revise before stopping.
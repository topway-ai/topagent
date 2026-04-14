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
- **Meaningful code change**: any non-trivial change that can affect runtime behavior, persistence, configuration, policy, prompt assembly, retrieval, tool surface, transport behavior, or tests relied on for correctness.
- **Hot path**: ordinary one-shot execution, ordinary Telegram handling, prompt assembly, bounded retrieval, preflight gating, and tool execution for a normal task.
- **Durable artifact**: any stored record expected to survive across sessions and influence future work, including USER.md, MEMORY.md, lessons, procedures, trajectories, checkpoints, and transcript stores.
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

- Hot-path cost must stay bounded as USER.md, MEMORY.md, lessons, procedures, trajectories, checkpoints, and transcripts grow.
- Durable artifact growth must not imply prompt growth.
- Retrieval must stay capped and relevance-filtered.
- Repeated tasks should get faster or more predictable from procedures and memory, not slower from extra ceremony or extra always-on reasoning.
- Security, provenance, and trust layers must use narrow boundary checks, summaries, and caps.
- Approval friction must stay risk-triggered.
- Do not add a second planner, second policy engine, second retrieval engine, or second always-running reasoning layer.
- Every change must say how repeat-task latency is preserved or improved, what stays capped, and why durable learning does not make the hot path grow linearly.

## Test Requirement
Any change that affects runtime behavior, persistence, retrieval, hooks, approvals, promotion, transport semantics, or the operational control plane must add or update tests.
If a meaningful change ships without tests, it must be explicitly labeled as a spike and must explain why the test gap is temporary.

## Documentation Sync
If a change alters operator-facing commands, lifecycle behavior, architecture ownership, or product boundary, update the relevant documentation in the same change unless the work is an explicitly labeled spike.

## Rules
1. Hot-path weight
2. Boundary integrity
3. Policy honesty
4. Session vs durable state
5. Memory quality
6. Compaction correctness
7. Approval clarity
8. Tool surface discipline
9. Transport separation
10. Restart-persistence necessity
11. Canonical artifact ownership
12. Bounded retrieval
13. Simplification vs exceptions
14. Product-shift honesty

(Keep your current 14 rule bodies; they are already strong.)

## Acceptable Complexity
(keep current section, lightly edited for clarity)

## Unacceptable Complexity
(keep current section, lightly edited for clarity)

## Spike Rule
Exploratory work is allowed only when it is explicitly labeled as a spike, narrow in scope, isolated from permanent architecture where possible, honest about what it is trying to learn, and easy to remove or replace.

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

If the result is not clearly simpler, more explicit, or more honestly justified, revise before stopping.
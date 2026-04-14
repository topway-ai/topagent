# Agent Instructions

`AGENTS.md` is the entry gate for meaningful code changes.
`REVIEW_RULES.md` is the authoritative review policy.

A meaningful code change is any non-trivial change that can affect runtime behavior, persistence, configuration, policy, prompt assembly, retrieval, tool surface, transport behavior, or tests relied on for correctness.

Before editing code:
1. Read `REVIEW_RULES.md`.
2. Produce a `Preflight Review` using the exact headings required there.
3. If the work adds complexity, explain why it is earned now and which simpler options were rejected.
4. If the work is exploratory, label it explicitly as a spike and state the question, containment, and success/failure criteria.
5. If the simplicity score is under 7/10, narrow the plan, justify the cost more clearly, or explicitly contain the work as a spike before editing.
6. Performance guardrails in `REVIEW_RULES.md` are mandatory. If the design risks unbounded hot-path growth, stop and narrow it before coding. If it adds durable artifacts, show why retrieval and prompt cost stay bounded.
7. If the change affects behavior, persistence, retrieval, hooks, approvals, promotion, transport semantics, or the operational control plane, add or update tests unless the work is an explicitly labeled spike.

After code changes, before stopping:
1. Produce a `Post-change Review` using the exact headings required in `REVIEW_RULES.md`.
2. State whether the added complexity was justified.
3. State whether `REVIEW_RULES.md` still fits the product boundary.
4. Update relevant documentation in the same change when operator-facing behavior, architecture ownership, or the product boundary changed, unless the work is an explicitly labeled spike.
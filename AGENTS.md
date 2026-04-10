# Agent Instructions

Before any meaningful code change:
1. Read `REVIEW_RULES.md`.
2. Produce a short Preflight Review using its required fields.
3. If the work adds complexity, explain why it is earned now and what simpler options were rejected.
4. If the work is exploratory, mark it explicitly as a spike and state containment plus success criteria.
5. If the simplicity score is under 7/10, narrow the plan, justify the cost more clearly, or explicitly contain the work as a spike before editing.
6. Performance guardrails in `REVIEW_RULES.md` are mandatory: if the design risks unbounded hot-path growth, stop and narrow it before coding; if it adds durable artifacts, show why retrieval and prompt cost stay bounded.

After code changes:
1. Produce a short Post-change Review using `REVIEW_RULES.md`.
2. State whether the added complexity was justified and whether `REVIEW_RULES.md` still fits the product boundary.

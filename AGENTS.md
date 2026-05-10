# Agent Instructions

`AGENTS.md` is the entry gate for meaningful code changes.
`REVIEW_RULES.md` is the authoritative review policy.

A meaningful code change is any non-trivial change that can affect runtime behavior, persistence, configuration, policy, prompt assembly, retrieval, tool surface, transport behavior, operator-visible behavior, or tests relied on for correctness.

## Before editing code

1. Read `REVIEW_RULES.md`.
2. Produce a `Preflight Review` using the exact headings required there.
3. If the work adds complexity, explain why it is earned now and which simpler options were rejected.
4. If the work is exploratory, label it explicitly as a spike and state the question, containment, and success/failure criteria.
5. If the simplicity score is under 7/10, narrow the plan, justify the cost more clearly, or explicitly contain the work as a spike before editing.
6. Performance guardrails in `REVIEW_RULES.md` are mandatory.
7. If the design risks unbounded hot-path growth, stop and narrow it before coding.
8. If it adds durable artifacts, show why retrieval and prompt cost stay bounded.
9. Before making changes, explicitly review whether related documentation and related tests need to be added, updated, removed, or left unchanged. If unchanged, say why.

## During the change

1. Keep behavior explicit in code or typed state when correctness, trust, safety, persistence, approvals, or policy are involved.
2. Keep transport/rendering concerns separate from runtime policy and runtime state.
3. Keep session state separate from durable memory.
4. Prefer extending an existing clear owner over creating a second owner, second truth source, or second always-running subsystem.
5. Keep the hot path bounded. Do not make ordinary runs heavier just because durable artifacts accumulated.
6. If the change affects runtime behavior, persistence, retrieval, hooks, approvals, promotion, transport semantics, or the operational control plane, add or update tests unless the work is an explicitly labeled spike.
7. If the change alters operator-facing commands, lifecycle behavior, architecture ownership, or product boundary, update the relevant documentation in the same change unless the work is an explicitly labeled spike.
8. If the change affects task execution, workflow completion, verification, receipts, tool-result reporting, or proof-of-work, keep the evidence model explicit in typed state and update tests that prove incomplete or unverified work cannot be reported as complete.
9. If the change affects prompt context, provider tools/tool schemas, memory injection, receipt handling, workflow verification, or final proof reporting, update tests and explicitly consider context-density cost.

## After code changes, before stopping

1. Produce a `Post-change Review` using the exact headings required in `REVIEW_RULES.md`.
2. State whether the added complexity was actually justified.
3. State whether `REVIEW_RULES.md` still fits the product boundary.
4. State explicitly whether related documentation was updated, removed, or intentionally left unchanged, and why.
5. State explicitly whether related tests were added, updated, removed, or intentionally left unchanged, and why.
6. If docs or tests should have changed but did not, do not treat the task as complete unless the work is an explicitly labeled spike.
7. If the change affects workflow receipts or verification state, state whether the result can still prove what was attempted, what was blocked, what failed, and what actually verified the outcome.
8. If the change affects prompt context or tool schema surface, state whether the relevant context/tool budget remains bounded or why a budget increase was justified.

# Reviewing Changes

TopAgent review should protect a specific product shape:

- Telegram-first, CLI-backed
- local-first and workspace-scoped
- operator-centric
- single-agent unless there is a very strong reason not to be
- explicit in behavior policy, approval, compaction, and durable memory

Use the PR template for the mechanical pass, then use this guide for judgment.

## What Reviewers Are Protecting

Reviewers are not mainly protecting style. They are protecting the repo from getting heavier, blurrier, and more exception-driven.

The common failure mode is not one bad feature. It is slow architecture drift:

- hot-path work grows a little at a time
- important behavior moves from code into prompt prose
- transport logic leaks into core runtime
- session state quietly becomes durable memory
- new files and helpers appear without a clear artifact owner

## How To Use The PR Template

Ask the author to fill the template concretely, not with "N/A" everywhere.

Use the template to find where to dig:

- if hot-path weight changed, inspect the agent loop, prompt build, compaction path, memory briefing path, and Telegram handling
- if boundary integrity changed, inspect whether code moved between `topagent-core`, `topagent-cli`, and workspace memory files cleanly
- if policy honesty changed, ask where the behavior is enforced in code
- if restart semantics changed, ask what correctness problem actually required new persistence

## Red Flags

- The hot path got larger, but the PR cannot name the frequent-load artifacts that grew.
- A rule now exists mostly in prompt prose instead of the behavior contract or runtime code.
- A change mixes session-only state with durable memory or config.
- Durable memory got bigger, but not more trustworthy.
- A new tool was added because it was convenient, not because the existing tool surface was insufficient.
- Telegram or CLI rendering logic leaked into `topagent-core`.
- The PR introduces persistence "for robustness" without a concrete restart-correctness need.
- One fact now has multiple owners: transcript, memory index, prompt summary, and runtime state all trying to say the same thing.
- The orchestration hotspot got another special case instead of losing one.

## Green Flags

- Behavior became more explicit in code and less dependent on giant prompt text.
- A change reduced the amount of state the model must reconstruct from transcript history.
- Durable memory became smaller, more curated, or more inspectable.
- A transport-specific behavior stayed in CLI or Telegram code while the policy-shaped part moved under the shared contract.
- A new artifact clearly owns one concern and replaces duplicated copies elsewhere.
- The PR deletes conditionals, prompt fragments, or parallel policy paths.

## Review Lenses

### 1. Hot-Path Weight

Check whether the PR increased work on every run or every model turn.

TopAgent hot-path artifacts include:

- system prompt assembly
- behavior contract rendering
- approval checks
- compaction state rebuild
- workspace memory briefing
- transcript retrieval
- Telegram polling and per-message handling

If one of these grew, the PR should say why the weight is justified.

### 2. Boundary Integrity

TopAgent stays cleaner when these boundaries remain sharp:

- `topagent-core`: agent loop, tool execution, policy, approval, compaction
- `topagent-cli`: CLI, Telegram runtime, service management, workspace memory plumbing
- workspace artifacts: plans, lessons, memory index, topic files, chat history

Reject changes that casually cross those boundaries.

### 3. Policy Honesty

Ask: is the behavior enforced, or merely described?

Good changes put important behavior in:

- explicit structs and enums
- runtime gates
- shared helpers
- inspectable artifacts

Be suspicious of changes that only add or edit prompt prose for behavior that should be enforced in code.

### 4. Runtime State Discipline

Session-only state should stay session-only:

- current objective
- current plan execution state
- blockers
- pending approvals
- active files
- recent proof-of-work

Do not let these leak into durable memory unless the repo already has a clear durable owner for them.

### 5. Memory Quality

Durable memory should get more useful, not just larger.

Check whether the change improves:

- explicit promotion
- merge behavior
- stale fact pruning
- inspectability
- stable preference handling

Reject append-only memory growth that just stores more text.

### 6. Compaction Correctness

Compaction must not silently lose:

- objective
- authoritative plan
- unresolved blockers
- pending approvals
- recent approval decisions that still constrain the run
- active file focus
- proof-of-work anchors

If compaction-related behavior changed, ask what canonical artifact now owns each preserved fact.

### 7. Approval Clarity

Approval should remain one real control path, not scattered "ask first" language.

Check that:

- risky actions still go through one decision point
- approval cannot be bypassed by an alternate tool path
- Telegram and CLI remain thin approval surfaces, not policy owners

### 8. Tool Surface Discipline

TopAgent should not grow tools casually.

A new tool should have a narrow reason:

- existing tools cannot express the operation cleanly
- the operation is stable enough to deserve a name
- the tool reduces ambiguity or policy drift

If it only wraps a tiny variant of an existing path, push back.

### 9. Transport Separation

Telegram-first does not mean Telegram logic belongs everywhere.

Check that:

- reply formatting stays out of core runtime
- polling and chat UX stay in CLI/Telegram code
- transport-specific persistence does not become a hidden core dependency

### 10. Restart Semantics

Persistent state should exist because restart correctness requires it, not because it feels safer.

Ask:

- what breaks today without this persistence?
- does the repo already support restart-resume for that state?
- can the state be rebuilt from canonical artifacts instead?

### 11. Canonical Artifact Ownership

One important fact should have one obvious owner.

Examples:

- plan state -> plan artifact/runtime plan
- approval state -> approval mailbox
- durable preference -> durable memory artifact
- transcript evidence -> Telegram history file

If the PR makes ownership less obvious, it is probably moving in the wrong direction.

### 12. Simplification Vs Added Complexity

Do not accept speculative complexity just because it is tidy in the abstract.

Prefer changes that:

- delete branches
- remove duplicated logic
- narrow prompts
- make policy/state easier to inspect

Push back when a PR adds framework-like structure without a current repo-specific need.

## Reviewer Test

Does this change make TopAgent more like a stable kernel, or more like a pile of accumulating exceptions?

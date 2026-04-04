See [docs/reviewing.md](docs/reviewing.md) for the repo-specific reviewer guide. Keep this PR body concrete and short.

## What changed

- Summary:
- Main files/modules:
- Why this change exists:

## Product boundary check

- [ ] Still fits TopAgent as Telegram-first, CLI-backed, local-first, workspace-scoped, operator-centric, single-agent
- [ ] Does not quietly turn TopAgent into a generic framework, workflow engine, or persistence platform
- Notes:

## Hot-path weight

- Frequent-load artifacts changed:
- [ ] Agent loop / prompt build / compaction / memory briefing / Telegram hot path did not get heavier without a concrete reason
- If hot-path weight grew, why:

## Boundary integrity

- Boundaries touched:
- [ ] `topagent-core`, `topagent-cli`, and workspace artifacts still have clear ownership
- Any layer mixing introduced:

## Policy honesty

- [ ] Important behavior is enforced in code or explicit policy structs, not only described in prompt prose
- New behavior owner:

## Runtime state discipline

- Session-only state touched:
- [ ] No current objective / blocker / plan / approval / active-run state leaked into durable memory without a clear durable owner
- Notes:

## Memory quality

- Durable memory artifacts touched:
- [ ] Memory got more useful or more curated, not just larger
- Promotion / merge / prune impact:

## Compaction correctness

- [ ] Compaction still preserves objective, authoritative plan, unresolved blockers, pending approvals, active file focus, and proof-of-work anchors
- Preserved state changed:

## Approval clarity

- [ ] Approval still has one real enforcement path and cannot be bypassed by the change
- Approval surface touched:

## Tool surface discipline

- New or changed tools:
- [ ] Any new tool is narrower and clearer than reusing the existing tool surface
- Justification:

## Transport separation

- [ ] Telegram/CLI rendering stayed out of core runtime unless the policy-shaped part was explicitly extracted
- Transport-specific logic added:

## Restart semantics

- New persistence added:
- [ ] Restart persistence is only added where correctness actually requires it
- Why rebuild from canonical artifacts was or was not sufficient:

## Canonical artifact ownership

- Important facts/artifacts changed:
- [ ] Each important fact still has one obvious canonical owner
- Ownership notes:

## Simplicity score

- Score (`-2` much simpler, `0` neutral, `+2` meaningfully more complex):
- Why:

## Risk summary

- Main regression risks:
- What a reviewer should challenge hardest:

## Verification

- Commands/tests/docs checks run:
- Not run:

## Reviewer verdict

- [ ] Boundary-respecting and ready
- [ ] Needs simplification before merge
- [ ] Needs stronger enforcement / artifact ownership / state discipline before merge
- Reviewer notes:

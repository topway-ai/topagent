# Architecture

## Project structure

TopAgent is a Rust workspace with two crates:

```
topagent/
  crates/
    topagent-core/     # Agent loop, tools, provider seam, Telegram primitives
    topagent-cli/      # CLI binary, Telegram runtime, memory, service management
  scripts/
    install.sh         # One-line installer
```

The `topagent` binary is built from `topagent-cli`.

## topagent-core

The engine crate. No CLI or Telegram logic -- just the agent loop, tools, and provider interface.

| Module | Responsibility |
|--------|---------------|
| `agent` | Agent struct and orchestration shell; internal `agent/gates`, `agent/planning_gate`, `agent/tool_execution`, and `agent/run_loop` keep loop control, gate sequencing, planning state, and tool-result application narrow |
| `behavior` | Typed behavior contract and policy root; internal task/action/approval/durability/compaction modules keep runtime policy seams narrow |
| `approval` | Approval mailbox, request/state transitions, runtime approval enforcement objects |
| `compaction` | Layered transcript compaction and prompt rebuild support |
| `run_state` | In-run objective, changed/active file tracking, bash verification history, compact tool trace capture, baseline attribution, proof-of-work assembly; finalizes into `TaskResult` with `ExecutionSessionOutcome` on all terminal paths (completed, stopped, max-steps, failed) |
| `session` | Conversation history management, truncation |
| `message` | Message types (user, assistant, system, tool_request, tool_result) |
| `provider` | Provider trait, response types |
| `openrouter` | OpenRouter API implementation |
| `model` | ModelRoute |
| `runtime` | RuntimeOptions (step limits, timeouts, truncation thresholds) |
| `tools/` | Tool trait, ToolRegistry, built-in tools (read, write, edit, bash, git_*) |
| `tool_spec` | Tool specification (name, description, parameters) |
| `context` | ExecutionContext (workspace root, cancel token, secrets), ToolContext |
| `secrets` | SecretRegistry: value-based and pattern-based redaction |
| `plan` | Plan struct, TodoItem, task modes |
| `project` | Load `TOPAGENT.md` project instructions |
| `prompt` | Policy-driven system prompt rendering from the behavior contract, run state, plan, memory, and tool surface |
| `provenance` | Compact source/trust labels and low-trust promotion/action policy inputs |
| `channel/` | Telegram adapter and channel error types |
| `cancel` | CancellationToken for graceful shutdown |
| `progress` | Progress update types for UI feedback |
| `file_util` | File hashing for change detection |

## topagent-cli

The binary crate. Handles CLI parsing, user interaction, and service management.

| Module | Responsibility |
|--------|---------------|
| `main` | Entry point: parses CLI args, converts to params, dispatches to command handlers |
| `commands` | CLI command types, dispatch, and rendering; `commands/types` owns all clap definitions, `commands/dispatch` owns the top-level match, per-domain modules (`memory_cli`, `procedure_cli`) own CLI rendering, `config` and `run` own their subcommand handlers (the `run` module dispatches run snapshot subcommands: `run status`, `run diff`, `run restore`; trajectory commands live under `memory trajectory`), `oneshot` owns the one-shot runner, and `artifact_util` shares file-list and path-resolution helpers |
| `config` | CliParams struct, parameter validation, route/options construction |
| `operational_paths` | Shared config-home, service unit, and managed env path ownership for the operational control plane |
| `run_context` | Shared agent/provider/context assembly for one-shot CLI and Telegram runs |
| `telegram` | Telegram polling and per-chat management; `runtime` owns the polling loop, `session` owns per-chat running state and transcript store, `router` dispatches messages and callback queries, `commands` handles slash-command logic, `history` owns transcript I/O, `approval` handles callback data and keyboard rendering, `admission` gates DM access, and `delivery` sends messages |
| `memory` | Workspace memory facade; `memory/briefing` handles bounded prompt briefing, `memory/promotion` handles verified-task governance, and sibling modules keep procedures, trajectories, and consolidation file-backed and narrow |
| `service/` | Operational control plane split by ownership: `install` handles install/reconfigure prompts, `model` handles persisted model changes, `state` owns shared status/config reads, `lifecycle` owns systemd/status/uninstall, and `managed_env` owns the single managed env truth |
| `managed_files` | Managed file guards, env file I/O, safe file removal |
| `progress` | LiveProgress: CLI and Telegram progress formatting |

## Runtime flows

### One-shot task

```
CLI parses args
  -> resolve workspace, API key, model route
  -> build operator model briefing from .topagent/USER.md
  -> build workspace memory briefing from .topagent/MEMORY.md + relevant procedures + relevant workspace notes
  -> classify run-level trust context from operator instruction + loaded memory/transcript sources
  -> create ExecutionContext with workspace + cancel token + operator model + workspace memory briefing + trust context
  -> create Agent with provider + tools + options
  -> agent.run(ctx, instruction)
     -> load TOPAGENT.md
     -> render policy-driven system prompt (+ project instructions + workspace memory briefing + compact run-state artifacts)
     -> classify task complexity -> activate planning gate if non-trivial
     -> enter step loop:
        1. send conversation to LLM
        2. LLM returns text (final answer) or tool calls
        3. tool calls: run preflight (planning gate, verification gate, provenance-aware approval/memory enforcement)
        4. execute tool, record result in session
        5. if a fetch-like shell command introduced low-trust external content, keep that influence in run state
        5. repeat until text response or max steps
-> append proof-of-work (changed files, diff summary, trust notes when low-trust content shaped the run)
      -> for PlanAndExecute mode with files changed: append structured delivery summary with explicit verification status
   -> if the task was strongly verified, run the workspace promotion policy:
- save nothing, or
      - save/update a durable note, or
      - save/update a reusable procedure, or
     - emit a compact trajectory artifact, or
     - some narrow combination of the above
     - but refuse durable promotions that are still primarily driven by low-trust content
  -> print result
```

### Telegram bot

```
CLI parses args
  -> resolve config (token, API key, workspace, model)
  -> register secrets for redaction
  -> create TelegramAdapter (long-polling)
  -> create ChatSessionManager (per-chat running tasks + transcript store + workspace memory)
  -> enter polling loop:
     1. fetch new messages from Telegram API
     2. for each message:
        - /start, /help -> reply with config summary
        - /stop -> cancel running task for that chat
        - /reset -> clear persisted transcript for that chat
         - /approvals -> list pending approvals for this chat
         - /approve <id> -> approve a pending action
         - /deny <id> -> deny a pending action
         - text -> start_message:
          a. load `.topagent/MEMORY.md` (always)
          b. load matching `.topagent/procedures/*.md` files only if relevant, capped to a small subset
          c. load matching workspace notes (`.topagent/notes/*.md`) only if relevant
          d. search the saved Telegram transcript and extract targeted snippets only if useful
          e. build a fresh agent run with the operator model plus that memory briefing and the merged trust context
          f. append the filtered user-visible transcript to disk
          g. if the task was strongly verified, apply the same verified-task promotion policy used by one-shot runs
          h. send reply (split into chunks if >4000 chars)
     3. on polling error: retry with backoff
```

Each chat gets its own running task state. The raw transcript is persisted to `workspace/.topagent/telegram-history/chat-<chat_id>.json` and survives service restarts, but prompt assembly only retrieves targeted transcript snippets when useful.

### Service install flow

```
topagent install
  -> check systemd user services available
  -> check for existing managed files (refuse to overwrite non-managed files)
  -> resolve workspace (--workspace, existing config, or auto-create)
  -> prompt for provider, provider API key, provider-scoped model, Telegram bot token, and optional allowed DM username
  -> write env file to ~/.config/topagent/services/topagent-telegram.env (mode 0600, includes model + runtime settings)
  -> write systemd unit to ~/.config/systemd/user/topagent-telegram.service
  -> systemctl --user daemon-reload
  -> fresh install: systemctl --user enable --now topagent-telegram.service
  -> reconfigure existing install: systemctl --user enable topagent-telegram.service && systemctl --user restart topagent-telegram.service
```

### Secret, approval, and sandbox safety

Secrets are protected at multiple layers:

1. **Environment stripping**: secret env vars (`OPENROUTER_API_KEY`, `TELEGRAM_BOT_TOKEN`, etc.) are removed from child process environments before bash commands run

2. **Command blocking**: bash commands that dump env vars (`env`, `printenv`, `export`) or read known secret files are blocked before execution

3. **Output redaction**: tool output is scanned for registered secret values and common secret patterns (API keys, bot tokens, key=value assignments) and replaced with `[REDACTED_SECRET]`

4. **Filesystem sandboxing**: when bubblewrap (`bwrap`) is available, bash commands run in a sandbox with:
   - read-only access to system directories (`/usr`, `/bin`, `/lib`, `/etc`)
   - read-write access only to the workspace and `/tmp`
   - network access disabled (`--unshare-net`)

5. **Path validation**: file tools reject absolute paths and parent directory traversal (`../`)

6. **Reply redaction**: Telegram replies are scanned for secrets before sending

7. **Approval enforcement**: risky actions such as destructive bash commands and `git_commit` must pass the central approval gate before execution

8. **Provenance-aware trust boundaries**:
   - direct operator instructions, generated memory artifacts, transcripts, and fetched content are labeled at ingress with a small source/trust model
   - low-trust content can be summarized or analyzed as data, but risky actions and durable memory writes become stricter when that content materially influences the run
   - approvals mention the low-trust source briefly and concretely instead of failing silently
   - durable promotion is stricter than temporary planning: low-trust content can block `USER.md`, procedure promotion, and trajectory review/export

9. **Prompt rules**: the system prompt instructs the LLM to never reveal credentials

### Memory and persistence flow

TopAgent uses `.topagent/workspace-state.json` as the current workspace schema marker. Schema version `1` is the only accepted workspace-state model.

TopAgent uses three prompt-memory layers, plus procedures (governed reuse) and trajectories (export artifacts) as separate categories:

1. **Operator model**: `workspace/.topagent/USER.md`
   - stable operator preferences and collaboration habits only
   - loaded separately from workspace memory and capped tightly
   - not for repo facts, task state, or transcript recall
2. **Workspace index**: `workspace/.topagent/MEMORY.md`
   - one-line entries only
   - cheap enough to load at task start
   - points to durable artifacts instead of embedding large notes
3. **Workspace notes**: `workspace/.topagent/notes/*.md`
   - compact durable notes, loaded only when relevant
   - `topagent memory status` shows count as "Notes: N note(s)"

**Procedures** (`workspace/.topagent/procedures/*.md`) are reusable playbooks with explicit reuse/revision/supersession metadata. They are not always-loaded memory; active procedures load lazily when relevant, and superseded procedures are ignored.

**Trajectory records** (`workspace/.topagent/trajectories/*.json`) are compact structured records from strong verified runs. They carry provenance labels, stay off the prompt hot path by default, and are exported via explicit review.

**Raw transcript evidence** (`workspace/.topagent/telegram-history/`) stores searchable per-chat text exchanges, never replayed wholesale.

`/reset` deletes only the per-chat transcript file. It does not touch `MEMORY.md`, workspace notes, procedures, or trajectories.

Curated consolidation keeps the index practical:

- strong verified tasks can promote into notes or procedures when they have future value
- operator preferences live outside the workspace index, so repo memory and user memory do not share ownership
- procedures prefer governed reuse: proven reuse can keep, refine, supersede, disable, or later prune a playbook instead of piling up duplicates
- duplicate or conflicting durable entries are merged or pruned instead of accumulating forever
- provenance is tracked at run boundaries rather than per token; the goal is explainable trust gating, not a full lineage graph
- relative timestamps are normalized before durable promotion when TopAgent has enough evidence
- missing or unreadable note files are skipped during retrieval
- the index load path caps injected bytes so startup memory stays cheap
- trajectory review/export stays explicit and local; saved artifacts do not become a second prompt-memory system

### Performance invariants

- Always-loaded memory stays tiny and bounded. `USER.md` and `MEMORY.md` are capped briefings, not growing prompt dumps.
- Lazy retrieval stays capped. Relevant procedures, workspace notes, operator preferences, transcript snippets, and injected bytes all use fixed limits.
- Transcript use stays targeted. Prior chat is searched for narrow snippets only and is never replayed wholesale into the prompt.
- Procedures are a latency aid, not a ceremony layer. They are loaded sparsely, only when relevant, and superseded procedures stay off the hot path.
- Trajectories are export artifacts, not prompt memory. Saving more trajectories must not make normal task startup heavier.
- Provenance/trust metadata stays lightweight and attached at key boundaries only. It must not become deep always-on analysis over every artifact.
- Durable artifact count must not imply linear growth in prompt assembly cost, retrieval cost, approval checks, or planning work.

### Planning flow

1. Agent classifies incoming task as trivial or non-trivial
2. Non-trivial tasks activate the **planning gate**, which blocks mutation tools
3. Agent researches (reads files, checks git) while gate is active
4. Agent creates a plan via `update_plan` -> gate deactivates
5. Agent executes plan steps, updating status as it goes
6. If the agent fails to plan within budget (10 steps or 5 blocked attempts), the system generates a fallback plan automatically

Plans can still be saved manually when the task-specific checklist itself matters. Verified-task promotion now uses notes for facts and pitfalls, procedures for reusable workflows, and trajectories for compact export records.

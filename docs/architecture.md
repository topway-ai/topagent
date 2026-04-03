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
| `agent` | Agent struct, step loop, planning gates, tool dispatch, execution stages |
| `session` | Conversation history management, truncation |
| `message` | Message types (user, assistant, system, tool_request, tool_result) |
| `provider` | Provider trait, response types |
| `openrouter` | OpenRouter API implementation |
| `provider_factory` | Create provider from route config |
| `model` | ModelRoute, ProviderId |
| `runtime` | RuntimeOptions (step limits, timeouts, truncation thresholds) |
| `tools/` | Tool trait, ToolRegistry, built-in tools (read, write, edit, bash, git_*) |
| `tool_genesis` | Workspace tool genesis split into storage core, generated-tool tools, and proposal tools |
| `tool_spec` | Tool specification (name, description, parameters) |
| `context` | ExecutionContext (workspace root, cancel token, secrets), ToolContext |
| `secrets` | SecretRegistry: value-based and pattern-based redaction |
| `plan` | Plan struct, TodoItem, task modes |
| `project` | Load `TOPAGENT.md` project instructions |
| `prompt` | System prompt construction |
| `external` | Workspace external tool registry loaded from `.topagent/external-tools.json` (and legacy `commands.json`) |
| `hooks` | Pre/post tool hooks |
| `channel/` | Telegram adapter and channel error types |
| `cancel` | CancellationToken for graceful shutdown |
| `progress` | Progress update types for UI feedback |
| `file_util` | File hashing for change detection |

## topagent-cli

The binary crate. Handles CLI parsing, user interaction, and service management.

| Module | Responsibility |
|--------|---------------|
| `main` | CLI argument parsing (clap), command dispatch, one-shot runner |
| `config` | CliParams struct, parameter validation, route/options construction |
| `telegram` | Telegram polling loop, ChatSessionManager, per-chat transcript persistence |
| `memory` | Workspace memory index/topic loading, transcript evidence retrieval, lightweight consolidation |
| `service` | systemd service install/status/start/stop/restart/uninstall |
| `managed_files` | Managed file guards, env file I/O, safe file removal |
| `progress` | LiveProgress: CLI and Telegram progress formatting |

## Runtime flows

### One-shot task

```
CLI parses args
  -> resolve workspace, API key, model route
  -> build workspace memory briefing from .topagent/MEMORY.md + relevant topic files
  -> create ExecutionContext with workspace + cancel token + memory briefing
  -> create Agent with provider + tools + options
  -> agent.run(ctx, instruction)
     -> load TOPAGENT.md, workspace external tools, generated tools
     -> build system prompt (+ project instructions + workspace memory briefing)
     -> classify task complexity -> activate planning gate if non-trivial
     -> enter step loop:
        1. send conversation to LLM
        2. LLM returns text (final answer) or tool calls
        3. tool calls: run preflight (hooks, planning gate, verification gate)
        4. execute tool, record result in session
        5. repeat until text response or max steps
     -> append proof-of-work (changed files, diff summary)
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
        - text -> start_message:
          a. load `.topagent/MEMORY.md` (always)
          b. load matching `.topagent/topics/*.md` files only if relevant
          c. search the saved Telegram transcript and extract targeted snippets only if useful
          d. build a fresh agent run with that memory briefing
          e. append the filtered user-visible transcript to disk
          f. send reply (split into chunks if >4000 chars)
     3. on polling error: retry with backoff
```

Each chat gets its own running task state. The raw transcript is persisted to `workspace/.topagent/telegram-history/chat-<chat_id>.json` and survives service restarts, but it is no longer restored wholesale into a model session.

### Service install flow

```
topagent install
  -> check systemd user services available
  -> check for existing managed files (refuse to overwrite non-managed files)
  -> resolve workspace (--workspace, existing config, or auto-create)
  -> prompt for OpenRouter API key and Telegram bot token
  -> write env file to ~/.config/topagent/services/topagent-telegram.env (mode 0600)
  -> write systemd unit to ~/.config/systemd/user/topagent-telegram.service
  -> systemctl --user daemon-reload
  -> systemctl --user enable --now topagent-telegram.service
```

### Secret and sandbox safety

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

7. **Prompt rules**: the system prompt instructs the LLM to never reveal credentials

### Memory and persistence flow

TopAgent now uses three memory layers:

1. **Always-loaded index**: `workspace/.topagent/MEMORY.md`
   - one-line entries only
   - cheap enough to load at task start
   - points to topic files instead of embedding large notes
2. **Lazy topic files**: `workspace/.topagent/topics/*.md`
   - compact durable notes by concern (`architecture`, `security`, `runtime`, etc.)
   - loaded only when the current task overlaps the topic name/tags/summary
3. **Raw transcript evidence**: `workspace/.topagent/telegram-history/chat-<chat_id>.json`
   - searchable per-chat transcript
   - stores user-visible text exchanges, not tool chatter
   - never replayed in full by default; retrieval returns targeted snippets only

`/reset` deletes only the per-chat transcript file. It does not touch `MEMORY.md`, topic files, plans, or lessons.

Lightweight consolidation keeps the index practical:

- exact duplicate `MEMORY.md` entries are deduplicated
- missing or unreadable topic files are skipped during retrieval
- the index load path caps injected bytes so startup memory stays cheap

### Planning flow

1. Agent classifies incoming task as trivial or non-trivial
2. Non-trivial tasks activate the **planning gate**, which blocks mutation tools
3. Agent researches (reads files, checks git) while gate is active
4. Agent creates a plan via `update_plan` -> gate deactivates
5. Agent executes plan steps, updating status as it goes
6. If the agent fails to plan within budget (10 steps or 5 blocked attempts), the system generates a fallback plan automatically

Plans can be saved to `.topagent/plans/` for reuse. Lessons can be saved to `.topagent/lessons/`.

# Architecture

## Project structure

TopAgent is a Rust workspace with two crates:

```
topagent/
  crates/
    topagent-core/     # Agent engine, tools, providers, channels
    topagent-cli/      # CLI binary, Telegram loop, service management
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
| `tool_genesis` | Dynamic tool creation: design, propose, approve, implement, repair |
| `tool_spec` | Tool specification (name, description, parameters) |
| `context` | ExecutionContext (workspace root, cancel token, secrets), ToolContext |
| `secrets` | SecretRegistry: value-based and pattern-based redaction |
| `plan` | Plan struct, TodoItem, task modes |
| `project` | Load `TOPAGENT.md` project instructions |
| `prompt` | System prompt construction |
| `commands` | Custom command registry (loaded from `.topagent/commands.json`) |
| `external` | External tool trait and registry (for tool genesis tools) |
| `hooks` | Pre/post tool hooks |
| `channel/` | ChannelAdapter trait, TelegramAdapter (polling-based) |
| `cancel` | CancellationToken for graceful shutdown |
| `progress` | Progress update types for UI feedback |
| `file_util` | File hashing for change detection |

## topagent-cli

The binary crate. Handles CLI parsing, user interaction, and service management.

| Module | Responsibility |
|--------|---------------|
| `main` | CLI argument parsing (clap), command dispatch, one-shot runner |
| `config` | CliParams struct, parameter validation, route/options construction |
| `telegram` | Telegram polling loop, ChatSessionManager, per-chat history persistence |
| `service` | systemd service install/status/start/stop/restart/uninstall |
| `managed_files` | Managed file guards, env file I/O, safe file removal |
| `progress` | LiveProgress: CLI and Telegram progress formatting |

## Runtime flows

### One-shot task

```
CLI parses args
  -> resolve workspace, API key, model route
  -> create ExecutionContext with workspace + cancel token
  -> create Agent with provider + tools + options
  -> agent.run(ctx, instruction)
     -> load TOPAGENT.md, external tools, commands
     -> build system prompt
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
  -> create ChatSessionManager (per-chat agents + history)
  -> enter polling loop:
     1. fetch new messages from Telegram API
     2. for each message:
        - /start, /help -> reply with config summary
        - /stop -> cancel running task for that chat
        - /reset -> clear persisted history for that chat
        - text -> start_message:
          a. restore persisted history into agent session
          b. agent.run(ctx, message)
          c. persist updated history to disk
          d. send reply (split into chunks if >4000 chars)
     3. on polling error: retry with backoff
```

Each chat gets its own Agent instance. History is persisted to `workspace/.topagent/telegram-history/chat-<chat_id>.json` and survives service restarts.

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

### Persistence flow

Telegram chat history is persisted per-chat:

```
workspace/.topagent/telegram-history/chat-<chat_id>.json
```

Format: JSON with a version field and an array of messages (role + content). History is loaded when a new message arrives and saved after each agent run completes. The `/reset` bot command deletes the history file for that chat.

When conversation exceeds 100 messages, the oldest half is dropped (keeping the most recent 50), with a note about truncated messages inserted.

### Planning flow

1. Agent classifies incoming task as trivial or non-trivial
2. Non-trivial tasks activate the **planning gate**, which blocks mutation tools
3. Agent researches (reads files, checks git) while gate is active
4. Agent creates a plan via `update_plan` -> gate deactivates
5. Agent executes plan steps, updating status as it goes
6. If the agent fails to plan within budget (10 steps or 5 blocked attempts), the system generates a fallback plan automatically

Plans can be saved to `.topagent/plans/` for reuse. Lessons can be saved to `.topagent/lessons/`.

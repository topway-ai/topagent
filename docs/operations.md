# Operations

## Installation methods

### Release binary (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Downloads a precompiled binary for Linux x86_64, verifies its SHA-256 checksum, and places it in `~/.cargo/bin/`. If the terminal is interactive, it launches `topagent install` automatically.

### From source

```bash
TOPAGENT_INSTALL_USE_CARGO=1 curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Installs Rust if needed, then builds from the git repository. Requires a C compiler (`build-essential` on Debian/Ubuntu).

### Installer environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `TOPAGENT_INSTALL_USE_CARGO` | unset | Force cargo build instead of release download |
| `TOPAGENT_INSTALL_PATH` | unset | Build from a local repo checkout instead of git |
| `TOPAGENT_INSTALL_ROOT` | unset | Install to `$ROOT/bin/` instead of `~/.cargo/bin/` |
| `TOPAGENT_INSTALL_VERSION` | latest | Download a specific release version |
| `TOPAGENT_SKIP_SETUP` | unset | Skip the interactive `topagent install` after binary install |

## Setup: topagent install

```bash
topagent install
```

Interactive setup that configures and starts the Telegram background service.

**What it does:**

1. Checks that systemd user services are available
2. Checks for existing config files (refuses to overwrite files not created by TopAgent)
3. Resolves the workspace directory:
   - `--workspace` flag if provided
   - existing value from a previous install
   - otherwise creates `workspace/` next to the installed binary (or in the repo root if running from source)
4. Prompts for OpenRouter API key (pre-fills from `--api-key`, env var, or previous install)
5. Prompts for Telegram bot token (pre-fills from previous install)
6. Writes config file: `~/.config/topagent/services/topagent-telegram.env` (mode 0600)
7. Writes systemd unit: `~/.config/systemd/user/topagent-telegram.service`
8. Runs `systemctl --user daemon-reload && systemctl --user enable --now topagent-telegram.service`

**Re-running install** updates the config and restarts the service. It preserves values you don't change.

## Service lifecycle

The Telegram bot runs as a systemd user service named `topagent-telegram.service`.

### Commands

```bash
topagent status              # show config paths, service state, and recent logs
topagent service start       # start the service
topagent service stop        # stop the service
topagent service restart     # restart the service (keeps current config)
topagent service install     # install service without the full interactive flow
topagent service uninstall   # remove service and config, keep binary
topagent uninstall           # remove service, config, and installed binary
```

### What topagent status shows

- Whether setup and service are installed
- systemd service state (enabled, active/inactive/failed)
- Config file path, unit file path, workspace path, and configured model
- Hints when something is wrong (e.g., journal command to inspect logs)

### What topagent uninstall removes

1. Stops and disables the systemd service
2. Removes the unit file and env file
3. Removes the installed binary (only if it was placed by the installer; source-checkout binaries are preserved)

`topagent service uninstall` does steps 1-2 only.

Neither command removes the workspace directory, curated memory files, or chat transcripts. Delete those manually if needed.

## Workspace behavior

The workspace is the root directory the agent operates in. All file paths are relative to it.

| Mode | How workspace is resolved |
|------|--------------------------|
| One-shot (`topagent "task"`) | Current working directory, or `--workspace` |
| Telegram (`topagent install`) | Interactive prompt with default, or `--workspace` |
| Foreground Telegram (`topagent telegram`) | Current directory, or `--workspace` |

The workspace must exist and be a directory. The agent creates a `.topagent/` subdirectory inside it for plans, lessons, tools, memory files, and chat transcripts.

### .topagent/ directory

```
workspace/.topagent/
  MEMORY.md                  # thin workspace memory index (always loaded)
  topics/                    # compact durable topic notes (lazy loaded)
  plans/                      # saved plans (JSON)
  lessons/                    # saved lesson notes (JSON)
  tools/                      # generated custom tools (manifests + scripts)
  telegram-history/           # per-chat transcript evidence files (JSON)
  external-tools.json         # workspace external tool definitions (if present)
```

Created automatically as needed. Not removed by `topagent uninstall`.

If a generated tool has an invalid manifest, is missing `script.sh`, is missing its stored script hash, or its current `script.sh` no longer matches the verified hash, TopAgent keeps the artifact on disk but reports it as a workspace warning instead of silently loading it.

`.topagent/external-tools.json` is the only supported workspace external-tool config file.

External tool entries must declare the same centralized sandbox policy explicitly:

```json
[
  {
    "name": "repo_todos",
    "description": "search TODOs inside the repo",
    "command": "rg",
    "argv_template": ["TODO", "{path}"],
    "sandbox": "workspace"
  },
  {
    "name": "system_uptime",
    "description": "show host uptime",
    "command": "uptime",
    "argv_template": [],
    "sandbox": "host"
  }
]
```

If `sandbox` is omitted, TopAgent rejects the external-tool config. Generated tools do not have this toggle; they always use the workspace sandbox policy when `bwrap` is available.

## Persistence and reset

### Three memory layers

#### 1. Always-loaded memory index

`workspace/.topagent/MEMORY.md`

- Tiny by design
- One-line pointer entries only
- Safe to inject at task start
- Should reference topic files or durable facts, not transcript dumps

#### 2. Lazy topic files

`workspace/.topagent/topics/*.md`

- Store compact durable notes by concern
- Loaded only when the current task matches the topic
- Good fits: architecture, runtime behavior, security constraints, open issues
- Bad fits: shell logs, command dumps, transient plans, cheap repo summaries

#### 3. Raw Telegram transcript evidence

`workspace/.topagent/telegram-history/chat-<chat_id>.json`

- One file per chat
- Persists user-visible text exchanges across service restarts
- Searchable evidence layer only
- Not restored wholesale into model context
- Retrieval returns targeted snippets only when useful
- Trimmed to the most recent 100 persisted text messages

### Retrieval behavior

When a new Telegram message arrives, TopAgent:

1. Loads `MEMORY.md`
2. Selects only topic files whose topic/tags overlap the task
3. Searches the raw transcript only when the task appears to refer to prior chat context
4. Injects a small memory briefing that explicitly tells the model to treat memory as hints and re-check current code/runtime state

If memory conflicts with the current repo, runtime, config, or service state, the current state wins.

### `/reset`

`/reset` remains a per-chat transcript reset:

- Clears `workspace/.topagent/telegram-history/chat-<chat_id>.json`
- Clears any in-memory running state for that chat
- Does **not** remove `MEMORY.md`
- Does **not** remove topic files, plans, lessons, or tools

This keeps reset semantics simple and aligned with the current product shape.

### Curated consolidation / pruning

TopAgent keeps memory lightweight with a bounded consolidation step:

- saved plans and lessons can promote into the durable memory index when they have future value
- duplicate or stale durable entries are merged, rewritten, or pruned instead of accumulating forever
- transcript persistence strips tool chatter and other internal session noise
- topic loading and transcript loading both cap how much can enter prompt context
- the always-loaded index stays bounded; durable details remain in topic files or archived artifacts

### Plans and lessons

Plans and lessons are saved under `.topagent/plans/` and `.topagent/lessons/` respectively. These persist across runs and are not affected by `/reset`.

### Config

The env file at `~/.config/topagent/services/topagent-telegram.env` stores the API key, bot token, model, workspace path, tool-authoring mode, and runtime limits (`max_steps`, `max_retries`, `timeout_secs`). It has mode 0600 (owner-readable only). The installed systemd unit reads this env file at startup, so re-running `topagent install` updates the next service run without duplicating those settings in `ExecStart`.

## TOPAGENT.md

Place a `TOPAGENT.md` file in the workspace root to provide project-specific instructions. The agent reads it at the start of every task and includes it in the system prompt.

Use this for:
- coding conventions the agent should follow
- files or directories to avoid modifying
- preferred tools or commands for testing
- project-specific context the agent wouldn't otherwise know

## Troubleshooting

### topagent: command not found

The binary is in `~/.cargo/bin/`. Run:

```bash
source "$HOME/.cargo/env"
```

### Service fails to start

Check the journal:

```bash
journalctl --user -u topagent-telegram.service -n 50
```

Common causes:
- Invalid or expired API key
- Invalid bot token
- Workspace path no longer exists
- Another process using the same bot token (Telegram allows only one poller per token)

### Telegram webhook conflict

If the bot was previously used with a webhook, long-polling will fail. Remove the webhook:

```bash
curl "https://api.telegram.org/bot<YOUR_TOKEN>/deleteWebhook"
```

### Bot not responding

1. `topagent status` -- check if the service is running
2. If stopped, check journal for errors
3. `topagent service restart` -- restart the service
4. Verify the bot token is correct by sending `/start`

### Agent produces poor results

- Try a different model: `topagent install` then change the model, or use `--model` flag
- Enable generated-tool authoring explicitly when needed: `--tool-authoring on`
- Add a `TOPAGENT.md` with project-specific guidance
- Break large tasks into smaller, more specific instructions

### Sandbox warnings

If you see `bwrap unavailable` in the logs, workspace-sandboxed commands run without filesystem sandboxing. That includes `bash`, generated tools, and any external tool configured with `"sandbox": "workspace"`. Install bubblewrap for sandboxed execution:

```bash
sudo apt install -y bubblewrap
```

The agent works without bubblewrap, but sandboxing provides an additional safety layer.

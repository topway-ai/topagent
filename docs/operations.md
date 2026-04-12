# Operations

## Installation methods

### Release binary (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Downloads a precompiled binary for Linux x86_64, verifies its SHA-256 checksum, and places it in `~/.cargo/bin/`. If the terminal is interactive, it launches `topagent setup` automatically.

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
| `TOPAGENT_SKIP_SETUP` | unset | Skip the interactive `topagent setup` after binary install |

## Setup: topagent setup

```bash
topagent setup
```

Interactive setup that configures and starts the Telegram background service. `topagent install` remains available as an alias.

**What it does:**

1. Checks that systemd user services are available
2. Checks for existing config files (refuses to overwrite files not created by TopAgent)
3. Resolves the workspace directory:
   - `--workspace` flag if provided
   - existing value from a previous install
   - otherwise creates `workspace/` next to the installed binary (or in the repo root if running from source)
4. Prompts for OpenRouter API key (pre-fills from `--api-key`, env var, or previous install)
5. Prompts for the OpenRouter model unless `--model` was provided:
   - tries to fetch a short list of current top OpenRouter models
   - falls back to the cached list under `~/.config/topagent/cache/openrouter-models.json`
   - falls back again to a curated starter list when no live or cached list is available
   - always offers `Custom model ID (type manually)`
6. Prompts for Telegram bot token (pre-fills from previous install)
7. Writes config file: `~/.config/topagent/services/topagent-telegram.env` (mode 0600)
8. Writes systemd unit: `~/.config/systemd/user/topagent-telegram.service`
9. Runs `systemctl --user daemon-reload`, then:
   - on fresh setup: `systemctl --user enable --now topagent-telegram.service`
   - on re-running setup over an existing managed install: `systemctl --user enable topagent-telegram.service` and `systemctl --user restart topagent-telegram.service`

Model precedence during install is:

1. explicit `--model`
2. the interactive model selection
3. the previously persisted `TOPAGENT_MODEL`
4. the built-in TopAgent default model

**Re-running setup** still updates the managed config and explicitly restarts the service with that updated config, but you no longer need to use it just to switch models.

## Service lifecycle

The Telegram bot runs as a systemd user service named `topagent-telegram.service`.

### Commands

```bash
topagent status              # show config paths, service state, and recent logs
topagent model status        # show the configured default and effective OpenRouter model
topagent model set <id>      # update the configured OpenRouter model
topagent model pick          # pick the configured OpenRouter model interactively
topagent model list          # show cached top OpenRouter models
topagent model refresh       # refresh the cached top OpenRouter models
topagent memory status       # show operator/workspace learning artifact status
topagent procedure list      # list live procedures
topagent procedure show <id> # show one procedure
topagent procedure prune     # remove superseded and disabled procedures
topagent procedure disable <id> [--reason ...] # disable a procedure without deleting it
topagent trajectory list     # list saved trajectories
topagent trajectory show <id> # show one trajectory
topagent trajectory review <id> # mark a trajectory ready for export
topagent trajectory export <id> # export a reviewed trajectory
topagent service start       # start the service
topagent service stop        # stop the service
topagent service restart     # restart the service (keeps current config)
topagent checkpoint status   # show the latest workspace checkpoint
topagent checkpoint diff     # preview the restore diff for the latest checkpoint
topagent checkpoint restore  # restore the latest checkpoint and clear Telegram transcripts
topagent service install     # install service without the full interactive flow
topagent service uninstall   # remove service and config, keep binary
topagent uninstall           # remove service, config, and installed binary
```

### Model management

`topagent model status` reads the same managed env file that powers `topagent status`, then reports both the configured default model and the effective model for the current invocation.

`topagent model set <openrouter-model-id>` updates only `TOPAGENT_MODEL` inside the managed env file, preserves the other managed values, and automatically restarts the Telegram service when it is installed.

`topagent model pick` uses the same OpenRouter model discovery and fallback logic as setup: live top-model lookup first, then cached models, then the curated starter list, with a manual custom-model entry path.

`topagent model list` shows the cached OpenRouter starter list and marks the current configured model when it appears in that cache.

`topagent model refresh` fetches the current top OpenRouter models and stores them in `~/.config/topagent/cache/openrouter-models.json`. If live refresh fails and a cache already exists, TopAgent keeps the stale cache and tells you so.

### Workspace checkpoints

TopAgent now captures a lightweight workspace checkpoint automatically before `write`, `edit`, and risky shell mutations. Checkpoints are stored under `workspace/.topagent/checkpoints/` and keep only the original contents of files that were touched during that task, or the minimal broader workspace snapshot needed for an obvious shell rewrite.

`topagent checkpoint status` shows the latest saved checkpoint and the files it captured.

`topagent checkpoint diff` previews the current workspace against that checkpoint so you can inspect the rollback before applying it.

`topagent checkpoint restore` restores the latest checkpoint and clears persisted Telegram transcripts for that workspace so the next chat run does not reload stale file-state context.

### Provenance and trust boundaries

TopAgent now keeps a small provenance model for execution-relevant text:

- `operator_direct`: the current operator instruction
- `generated_memory_artifact`: `USER.md`, `MEMORY.md`, lessons, procedures, and other curated memory loaded into the run
- `transcript_prior`: prior Telegram snippets retrieved as evidence
- `fetched_web_content`: fetch-like shell commands that pulled in external content
- `pasted_untrusted_text`: obviously pasted or quoted external content in the current instruction

These labels are not a full lineage system. They are a small run-level trust summary used for two things:

- risky action approvals mention low-trust influence when it materially shaped the proposed action
- durable learning writes become stricter than temporary planning use

Low-trust content may still be summarized, quoted, or analyzed as data. It does not automatically become durable memory, operator preferences, reusable procedures, or export-ready trajectories.

### What topagent status shows

- Whether setup and service are installed
- systemd service state (enabled, active/inactive/failed)
- Config file path, unit file path, workspace path, configured default model, and effective model
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
| Telegram (`topagent setup`) | Interactive prompt with default, or `--workspace` |
| Foreground Telegram (`topagent telegram`) | Current directory, or `--workspace` |

The workspace must exist and be a directory. The agent creates a `.topagent/` subdirectory inside it for plans, lessons, tools, memory files, and chat transcripts.

### .topagent/ directory

```
workspace/.topagent/
  USER.md                    # operator model (stable user preferences)
  MEMORY.md                  # thin workspace memory index (always loaded)
  topics/                    # compact durable topic notes (lazy loaded)
  plans/                     # manual saved plans (markdown)
  lessons/                   # saved lesson notes (markdown)
  procedures/                # governed reusable procedures (markdown)
  trajectories/              # local saved trajectory records (JSON)
  exports/trajectories/      # reviewed trajectory export packages (JSON)
  checkpoints/               # automatic workspace checkpoints for restore
  tools/                     # generated custom tools (manifests + scripts)
  telegram-history/          # per-chat transcript evidence files (JSON)
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

### Governed learning layers

#### 1. Operator model

`workspace/.topagent/USER.md`

- Stores stable operator preferences and collaboration habits only
- Loaded separately from repo/workspace memory
- Kept small and capped tightly in the prompt
- Not used for repo facts, shell evidence, or task-local state

#### 2. Always-loaded memory index

`workspace/.topagent/MEMORY.md`

- Tiny by design
- One-line pointer entries only
- Safe to inject at task start
- Should reference topic files or durable facts, not transcript dumps

#### 3. Lazy durable workspace artifacts

`workspace/.topagent/topics/*.md`, `workspace/.topagent/lessons/*.md`, `workspace/.topagent/procedures/*.md`, `workspace/.topagent/plans/*.md`

- Store compact durable notes by concern
- Loaded only when the current task matches the topic
- Good fits: architecture, runtime behavior, security constraints, open issues
- Bad fits: shell logs, command dumps, transient plans, cheap repo summaries
- Procedures are the reusable workflow layer: they track reuse count, revision count, supersession, and disablement
- `topagent procedure list` shows live procedures by default
- `topagent procedure show <id>` shows the raw on-disk playbook
- `topagent procedure prune` removes superseded and disabled procedures
- `topagent procedure disable <id>` demotes a noisy procedure without deleting it immediately

#### 4. Raw Telegram transcript evidence

`workspace/.topagent/telegram-history/chat-<chat_id>.json`

- One file per chat
- Persists user-visible text exchanges across service restarts
- Searchable evidence layer only
- Not restored wholesale into model context
- Retrieval returns targeted snippets only when useful
- Trimmed to the most recent 100 persisted text messages

#### 5. Trajectory records and export packages

`workspace/.topagent/trajectories/*.json`

- Compact structured records from strong verified runs
- Saved locally first with review state `local_only`
- `topagent trajectory review <id>` runs the explicit readiness gate and marks the artifact `ready_for_export`
- `topagent trajectory export <id>` writes a copy to `workspace/.topagent/exports/trajectories/` and marks the local record `exported`
- Trajectory export refuses weak, unsafe, or still-low-trust artifacts
- Saved-local and exported trajectories are distinct states

### Retrieval behavior

When a new Telegram message arrives, TopAgent:

1. Loads the capped operator model from `USER.md`
2. Loads `MEMORY.md`
3. Selects only the small set of procedures and durable artifacts whose topic/tags overlap the task
4. Searches the raw transcript only when the task appears to refer to prior chat context
5. Carries a small trust summary alongside that memory so transcript/external content does not silently become trusted intent
6. Injects a small memory briefing that explicitly tells the model to treat memory as hints and re-check current code/runtime state

If memory conflicts with the current repo, runtime, config, or service state, the current state wins.

### `/reset`

`/reset` remains a per-chat transcript reset:

- Clears `workspace/.topagent/telegram-history/chat-<chat_id>.json`
- Clears any in-memory running state for that chat
- Does **not** remove `USER.md`
- Does **not** remove `MEMORY.md`
- Does **not** remove topic files, plans, lessons, procedures, trajectories, or tools

This keeps reset semantics simple and aligned with the current product shape.

### Curated consolidation / pruning

TopAgent keeps memory lightweight with a bounded consolidation step:

- saved lessons and procedures can promote into the durable memory index when they have future value
- operator preferences stay in `USER.md`; they do not belong to workspace memory
- procedure revision is governed by proven reuse rather than blind accumulation
- duplicate or stale durable entries are merged, rewritten, or pruned instead of accumulating forever
- low-trust transcript or external content can inform temporary planning, but durable promotion requires stronger corroboration
- transcript persistence strips tool chatter and other internal session noise
- topic loading and transcript loading both cap how much can enter prompt context
- the always-loaded index stays bounded; durable details remain in topic files or archived artifacts

### Plans and lessons

Plans and lessons are saved under `.topagent/plans/` and `.topagent/lessons/` respectively. These persist across runs and are not affected by `/reset`.

### Config

The env file at `~/.config/topagent/services/topagent-telegram.env` stores the API key, bot token, model, workspace path, tool-authoring mode, and runtime limits (`max_steps`, `max_retries`, `timeout_secs`). It has mode 0600 (owner-readable only). The installed systemd unit reads this env file at startup, so `topagent setup`, `topagent model set`, and `topagent model pick` all update the next service run without duplicating those settings in `ExecStart`.

The OpenRouter discovery cache lives separately at `~/.config/topagent/cache/openrouter-models.json`. It is only a convenience cache for install/list/refresh and is not the active model source of truth.

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

- Try a different model: `topagent model set <openrouter-model-id>`
- Refresh the cached starter list first when you want fresh options: `topagent model refresh`
- Use `--model <id>` for one-shot runs or foreground Telegram without changing the installed service
- Enable generated-tool authoring explicitly when needed: `--tool-authoring on`
- Add a `TOPAGENT.md` with project-specific guidance
- Break large tasks into smaller, more specific instructions

### Sandbox warnings

If you see `bwrap unavailable` in the logs, workspace-sandboxed commands run without filesystem sandboxing. That includes `bash`, generated tools, and any external tool configured with `"sandbox": "workspace"`. Install bubblewrap for sandboxed execution:

```bash
sudo apt install -y bubblewrap
```

The agent works without bubblewrap, but sandboxing provides an additional safety layer.

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

- Whether the service unit file and env file exist
- systemd service state (active, inactive, failed)
- Config file path and workspace path
- Last 15 lines of journal output

### What topagent uninstall removes

1. Stops and disables the systemd service
2. Removes the unit file and env file
3. Removes the installed binary (only if it was placed by the installer; source-checkout binaries are preserved)

`topagent service uninstall` does steps 1-2 only.

Neither command removes the workspace directory or chat history. Delete those manually if needed.

## Workspace behavior

The workspace is the root directory the agent operates in. All file paths are relative to it.

| Mode | How workspace is resolved |
|------|--------------------------|
| One-shot (`topagent "task"`) | Current working directory, or `--workspace` |
| Telegram (`topagent install`) | Interactive prompt with default, or `--workspace` |
| Foreground Telegram (`topagent telegram`) | Current directory, or `--workspace` |

The workspace must exist and be a directory. The agent creates a `.topagent/` subdirectory inside it for plans, lessons, tools, and chat history.

### .topagent/ directory

```
workspace/.topagent/
  plans/                      # saved plans (JSON)
  lessons/                    # saved lesson notes (JSON)
  tools/                      # generated custom tools (manifests + scripts)
  tool-genesis/proposals/     # tool proposals awaiting approval
  telegram-history/           # per-chat history files (JSON)
  commands.json               # custom command definitions (if present)
```

Created automatically as needed. Not removed by `topagent uninstall`.

## Persistence and reset

### Telegram chat history

Each Telegram chat has a separate history file at `workspace/.topagent/telegram-history/chat-<chat_id>.json`. History is loaded at the start of each message and saved after the agent finishes.

- History survives service restarts
- `/reset` in the Telegram chat clears the history for that chat
- Manually deleting the JSON file has the same effect as `/reset`
- When conversation exceeds 100 messages, the oldest half is dropped (keeping the most recent 50)

### Plans and lessons

Plans and lessons are saved under `.topagent/plans/` and `.topagent/lessons/` respectively. These persist across runs and are not affected by `/reset`.

### Config

The env file at `~/.config/topagent/services/topagent-telegram.env` stores the API key, bot token, provider, model, workspace path, and runtime options. It has mode 0600 (owner-readable only). Re-running `topagent install` overwrites it.

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
- Add a `TOPAGENT.md` with project-specific guidance
- Break large tasks into smaller, more specific instructions

### Sandbox warnings

If you see `bwrap unavailable` in the logs, bash commands run without filesystem sandboxing. Install bubblewrap for sandboxed execution:

```bash
sudo apt install -y bubblewrap
```

The agent works without bubblewrap, but sandboxing provides an additional safety layer.

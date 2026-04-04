# TopAgent

A Telegram-first, CLI-backed local coding agent that reads one repository, plans changes, and executes them with file tools and local shell commands.

Uses [OpenRouter](https://openrouter.ai/) for LLM access. Default model: `minimax/minimax-m2.7`.

## Install

Download the latest release binary (Linux x86_64):

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

The installer places `topagent` in `~/.cargo/bin/` and optionally launches the interactive setup.

To build from source instead:

```bash
TOPAGENT_INSTALL_USE_CARGO=1 curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

## Quick start: one-shot

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
cd /path/to/your/repo
topagent "summarize this repository"
```

Press Ctrl-C once to request a graceful stop. Press again to force exit.

## Quick start: Telegram bot

```bash
topagent install
```

This prompts for your OpenRouter API key and Telegram bot token (from [BotFather](https://t.me/BotFather)), then:

- creates a workspace directory for the agent to operate in
- writes a managed config file under `~/.config/topagent/`
- installs and starts a `topagent-telegram.service` systemd user service

Then open a private chat with your bot and send a message.

TopAgent keeps Telegram memory in three layers:

- a tiny always-loaded workspace index at `workspace/.topagent/MEMORY.md`
- compact durable notes under `workspace/.topagent/topics/`, plus archived lessons and saved plans under `workspace/.topagent/lessons/` and `workspace/.topagent/plans/`, loaded only when relevant
- a per-chat raw transcript under `workspace/.topagent/telegram-history/`, used as searchable evidence rather than replayed wholesale

### Bot commands

| Command  | Action                             |
|----------|------------------------------------|
| `/start` | Show configuration and help        |
| `/help`  | Same as /start                     |
| `/stop`  | Cancel the currently running task  |
| `/reset` | Clear this chat's saved transcript |
| `/tool_authoring on|off` | Enable or disable generated-tool authoring for this chat |

### Service management

```bash
topagent status              # show setup and service health
topagent service start       # start the background service
topagent service stop        # stop the background service
topagent service restart     # restart the background service
topagent uninstall           # remove service, config, and installed binary
```

Re-running `topagent install` updates the config and restarts the service.

See [docs/operations.md](docs/operations.md) for full operational details.

## Global flags

| Flag                  | Default        | Description                        |
|-----------------------|----------------|------------------------------------|
| `--api-key`           | `$OPENROUTER_API_KEY` | OpenRouter API key            |
| `--provider`          | `openrouter`   | LLM provider (OpenRouter only today) |
| `--model`             | `minimax/minimax-m2.7` | Model identifier (OpenRouter format) |
| `--workspace`         | current directory (one-shot) or auto-created (install) | Workspace path |
| `--max-steps`         | `50`           | Maximum agent loop iterations      |
| `--max-retries`       | `3`            | Maximum provider retry attempts    |
| `--timeout-secs`      | `120`          | Provider request timeout           |
| `--tool-authoring`    | `off`          | Enable or disable generated-tool authoring tools |

## Project instructions

Place a `TOPAGENT.md` file in your workspace root to give the agent project-specific guidance. The agent reads it automatically at the start of each task.

Workspace memory is separate from `TOPAGENT.md`:

- `TOPAGENT.md` is for always-on project instructions
- `.topagent/MEMORY.md` is a tiny durable memory index
- `.topagent/topics/`, `.topagent/lessons/`, and `.topagent/plans/` hold compact durable notes and archived artifacts that get curated back into the index when useful
- `.topagent/telegram-history/` stores searchable per-chat transcript evidence

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `topagent: command not found` | `source "$HOME/.cargo/env"` |
| `A C compiler is required` | `sudo apt install -y build-essential` |
| `OpenRouter API key required` | Set `--api-key` or `OPENROUTER_API_KEY` |
| `Workspace path does not exist` | Run from a repo, pass `--workspace`, or run `topagent install` |
| `Telegram bot token looks invalid` | Get a valid token from BotFather |
| `Telegram webhook is configured` | Remove the webhook, then retry |
| `systemd user services are unavailable` | Log into a desktop session where `systemctl --user` works |

## Current limitations

- Telegram: private chats only, text messages only
- One workspace per process
- OpenRouter is the only supported provider
- Linux only (systemd required for background service)

## Documentation

- [Overview](docs/overview.md) -- what TopAgent is, design goals, capabilities, limitations
- [Architecture](docs/architecture.md) -- crate structure, modules, runtime flows
- [Operations](docs/operations.md) -- install, service lifecycle, persistence, troubleshooting

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
topagent setup
```

This prompts for your OpenRouter API key, lets you pick an OpenRouter model (or type a custom model ID), and asks for your Telegram bot token (from [BotFather](https://t.me/BotFather)), then:

- creates a workspace directory for the agent to operate in
- writes a managed config file under `~/.config/topagent/`
- installs and starts a `topagent-telegram.service` systemd user service

Then open a private chat with your bot and send a message.

TopAgent keeps Telegram memory in three layers:

- a small operator model at `workspace/.topagent/USER.md` for stable collaboration preferences, loaded separately and capped tightly
- a tiny always-loaded workspace index at `workspace/.topagent/MEMORY.md`
- compact durable notes under `workspace/.topagent/topics/`, plus archived lessons, reusable procedures, and saved plans under `workspace/.topagent/lessons/`, `workspace/.topagent/procedures/`, and `workspace/.topagent/plans/`, loaded only when relevant
- a per-chat raw transcript under `workspace/.topagent/telegram-history/`, used as searchable evidence rather than replayed wholesale

For strong verified runs, TopAgent can also emit compact trajectory artifacts under `workspace/.topagent/trajectories/`. These are structured export records for later eval or training work, not prompt memory, and they stay local until reviewed and exported explicitly.

TopAgent also keeps a narrow trust boundary for external content:

- direct operator intent and current workspace state are the normal trusted path
- saved memory and procedures are advisory artifacts, not ground truth
- prior transcripts, pasted external text, and fetched web content are treated as low-trust inputs
- low-trust content can still be analyzed as data, but risky actions and durable memory writes get stricter gating when that content materially influences the run
- TopAgent does not claim to solve prompt injection; it only keeps provenance explicit enough to avoid silent promotion or silent risky-action drift

### Bot commands

| Command  | Action                             |
|----------|------------------------------------|
| `/start` | Show configuration and help        |
| `/help`  | Same as /start                     |
| `/stop`  | Cancel the currently running task  |
| `/reset` | Clear this chat's saved transcript |

### Service management

```bash
topagent status              # show setup and service health
topagent model status        # show the configured default and effective OpenRouter model
topagent model set <id>      # change the configured OpenRouter model
topagent model pick          # pick the configured OpenRouter model interactively
topagent model list          # show cached starter models
topagent model refresh       # refresh cached starter models
topagent memory status       # show operator/workspace learning artifact status
topagent procedure list      # list live procedures
topagent procedure show <id> # show one procedure
topagent procedure prune     # remove superseded and disabled procedures
topagent trajectory list     # list saved trajectories
topagent trajectory show <id> # show one trajectory
topagent trajectory review <id> # mark a trajectory ready for export
topagent trajectory export <id> # export a reviewed trajectory
topagent service start       # start the background service
topagent service stop        # stop the background service
topagent service restart     # restart the background service
topagent checkpoint status   # show the latest workspace checkpoint
topagent checkpoint diff     # preview what restore would change
topagent checkpoint restore  # restore the latest checkpoint and clear Telegram transcripts
topagent uninstall           # remove service, config, and installed binary
```

`topagent setup` is the obvious full setup path. `topagent install` remains available as the same command. Re-running setup keeps the same managed config file and restarts the background service with updated values. After setup, use `topagent model set` or `topagent model pick` to change the configured default model without re-running full setup.

See [docs/operations.md](docs/operations.md) for full operational details.

## Global flags

| Flag                  | Default        | Description                        |
|-----------------------|----------------|------------------------------------|
| `--api-key`           | `$OPENROUTER_API_KEY` | OpenRouter API key            |
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
- `.topagent/USER.md` is for stable operator preferences and collaboration habits that should not be mixed into repo memory
- `.topagent/MEMORY.md` is a tiny durable memory index
- `.topagent/topics/` holds compact durable notes by concern
- `.topagent/lessons/` holds distilled facts, pitfalls, and rules from verified work
- `.topagent/procedures/` holds reusable workspace-local playbooks distilled from strong verified runs, revised through proven reuse, and loaded lazily in small batches
- `.topagent/plans/` holds manual saved plans; auto-promotion no longer uses plans as the reusable workflow artifact
- `.topagent/trajectories/` holds compact structured execution traces from high-quality verified runs; they are reviewable export artifacts, not hot-path prompt memory
- `.topagent/exports/trajectories/` holds reviewed trajectory export packages
- `.topagent/telegram-history/` stores searchable per-chat transcript evidence
- `.topagent/checkpoints/` stores the most recent automatic workspace checkpoints for restore

TopAgent does not promote every successful task. Weak, trivial, failed, or ambiguous runs save nothing. It still does not provide a skills marketplace, subagents, online training, or multi-provider routing.

Saved trajectories now include provenance labels from the run. A trajectory can still be stored locally with low-trust influence for audit value, but `topagent trajectory review` and `topagent trajectory export` refuse artifacts that remain influenced by unresolved low-trust content.

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `topagent: command not found` | `source "$HOME/.cargo/env"` |
| `A C compiler is required` | `sudo apt install -y build-essential` |
| `OpenRouter API key required` | Set `--api-key` or `OPENROUTER_API_KEY` |
| `Workspace path does not exist` | Run from a repo, pass `--workspace`, or run `topagent setup` |
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
- [Review Rules](REVIEW_RULES.md) -- short LLM preflight and post-change checks before meaningful code changes

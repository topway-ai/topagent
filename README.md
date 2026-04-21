# TopAgent

A Telegram-first, CLI-backed local coding agent that reads one repository, plans changes, and executes them with file tools and local shell commands.

Supports two LLM providers through one shared OpenAI-compatible transport seam:
- **OpenRouter** (default) — default model: `minimax/minimax-m2.7`
- **Opencode** — default model: `glm-5.1`

Provider is selected explicitly during setup.

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
# or: export OPENCODE_API_KEY="your_opencode_key"
cd /path/to/your/repo
topagent "summarize this repository"
# or with Opencode: topagent --model glm-5.1 "summarize this repository"
```

Press Ctrl-C once to request a graceful stop. Press again to force exit.

## Quick start: Telegram bot

```bash
topagent setup
```

This prompts for:
1. **Provider** — Choose OpenRouter or Opencode
2. **API key** — For the selected provider
3. **Model** — Choose from the provider's model list, or enter a custom model ID
4. **Telegram bot token** — From [BotFather](https://t.me/BotFather)
5. **Allowed Telegram username** — Optional; the username (without `@`) of the user allowed to send direct messages to the bot

Then it:
- creates a workspace directory for the agent to operate in
- writes a managed config file under `~/.config/topagent/`
- installs and starts a `topagent-telegram.service` systemd user service

**Telegram access control**: If you enter an allowed username during setup, the bot will only accept direct messages from that user. The first direct message from the allowed username binds and persists the numeric Telegram user ID. After binding, enforcement switches to numeric user ID — so username changes won't break access. Non-private chats (groups, channels, supergroups) are rejected before any binding side effect, so an allowed username cannot accidentally bind to a group's chat ID.

Then open a private chat with your bot and send a message.

TopAgent uses three prompt-memory layers:

1. **Operator model** — `workspace/.topagent/USER.md` stores stable collaboration preferences; loaded separately and capped tightly
2. **Workspace index** — `workspace/.topagent/MEMORY.md` is a tiny always-loaded pointer index
3. **Workspace notes** — `workspace/.topagent/notes/` holds compact durable notes loaded only when relevant

Procedures (`workspace/.topagent/procedures/`) are reusable playbooks governed by proven reuse, not prompt memory. Trajectories (`workspace/.topagent/trajectories/`) are structured export records, not prompt memory. Both stay off the hot path by default.

A per-chat raw transcript under `workspace/.topagent/telegram-history/` is searchable evidence, never replayed wholesale.

TopAgent also keeps a narrow trust boundary for external content:

- direct operator intent and current workspace state are the normal trusted path
- saved memory and procedures are advisory artifacts, not ground truth
- prior transcripts, pasted external text, and fetched web content are treated as low-trust inputs
- low-trust content can still be analyzed as data, but risky actions and durable memory writes get stricter gating when that content materially influences the run
- TopAgent does not claim to solve prompt injection; it only keeps provenance explicit enough to avoid silent promotion or silent risky-action drift

### Bot commands

| Command | Action |
|---------|--------|
| `/start` | Show configuration and help |
| `/help` | Same as /start |
| `/stop` | Cancel the currently running task |
| `/approvals` | List pending approvals for this chat |
| `/approve <id>` | Approve a pending action |
| `/deny <id>` | Deny a pending action |
| `/reset` | Clear this chat's saved transcript |

### Service management

```bash
topagent status                # Is the service installed and running?
topagent model status # show the configured default and effective model
topagent model set <id> # change the configured model (does not change provider; --model overrides for one-shot only)
topagent model pick # pick the configured model interactively
topagent model list # show cached top models
topagent model refresh # refresh cached top models
topagent memory status       # show notes, procedures, and trajectory counts
topagent memory lint         # lint USER.md and MEMORY.md for size and content policy issues
topagent memory recall "..." # dry-run memory retrieval for an instruction
topagent memory trajectory list     # list saved trajectories
topagent memory trajectory show <id> # show one trajectory
topagent memory trajectory review <id> # mark a trajectory ready for export
topagent memory trajectory export <id> # export a reviewed trajectory
topagent procedure list      # list live procedures
topagent procedure show <id> # show one procedure
topagent procedure prune     # remove superseded and disabled procedures
topagent procedure disable <id> # disable a procedure without deleting it
topagent telegram              # run the Telegram bot in the foreground
topagent service install     # install service without the full interactive flow
topagent service start       # start the background service
topagent service stop        # stop the background service
topagent service restart     # restart the background service
topagent service uninstall   # stop service, remove unit+env files, keep binary
topagent run diff            # preview what restore would change
topagent run restore         # restore the latest checkpoint and clear Telegram transcripts
topagent config inspect      # What provider/model/keys am I actually using?
topagent run status          # What happened in my last run? (checkpoint, transcripts, recovery guidance)
topagent doctor              # Is everything healthy? (deep diagnostics)
topagent upgrade             # download and install the latest GitHub release binary
topagent upgrade --use-cargo # build and install from source via cargo instead of a release binary
topagent uninstall           # stop service, remove unit+env files, optionally remove binary
topagent uninstall --purge   # also remove .topagent/ workspace data and model cache (neither removes the workspace directory)
```

`topagent setup` is the obvious full setup path. `topagent install` remains available as the same command. Re-running setup keeps the same managed config file and restarts the background service with updated values; operator-entered secrets (API keys, bot token, allowed username, bound user ID) are preserved when you accept the existing prompt defaults, and the env file is rewritten in a single atomic emit rather than overwritten twice. After setup, use `topagent model set` or `topagent model pick` to change the configured default model without re-running full setup. `model set` changes only the model, not the provider — to change provider, re-run `topagent setup`. The `--model` flag overrides the configured default for one-shot runs only, without changing the persisted config.

See [docs/operations.md](docs/operations.md) for full operational details.

## Global flags

| Flag | Default | Description |
|-----------------------|----------------|------------------------------------|
| `--api-key` | `$OPENROUTER_API_KEY` | API key for the selected provider (or use `--opencode-api-key` for Opencode) |
| `--opencode-api-key` | `$OPENCODE_API_KEY` | Opencode API key |
| `--model` | `minimax/minimax-m2.7` | Model identifier; overrides the configured default for one-shot only (use `model set` to persist) |
| `--workspace` | current directory (one-shot) or auto-created (install) | Workspace path |
| `--max-steps` | `50` | Maximum agent loop iterations |
| `--max-retries` | `10` | Maximum provider retry attempts |
| `--timeout-secs` | `120` | Provider request timeout |
| `--tool-authoring` | `off` | Enable or disable generated-tool authoring tools |

## Project instructions

Place a `TOPAGENT.md` file in your workspace root to give the agent project-specific guidance. The agent reads it automatically at the start of each task.

Workspace memory is separate from `TOPAGENT.md`:

- `TOPAGENT.md` is for always-on project instructions
- `.topagent/USER.md` is for stable operator preferences and collaboration habits that should not be mixed into repo memory
- `.topagent/MEMORY.md` is a tiny durable memory index
- `.topagent/notes/` holds workspace notes — compact durable notes loaded only when relevant
- `.topagent/procedures/` holds reusable workspace-local playbooks distilled from strong verified runs, revised through proven reuse, and loaded lazily in small batches
- `.topagent/trajectories/` holds compact structured execution traces from high-quality verified runs; they are reviewable export artifacts, not hot-path prompt memory
- `.topagent/exports/trajectories/` holds reviewed trajectory export packages
- `.topagent/telegram-history/` stores searchable per-chat transcript evidence
- `.topagent/checkpoints/` stores the most recent automatic workspace checkpoints for restore

TopAgent does not promote every successful task. Weak, trivial, failed, or ambiguous runs save nothing. It still does not provide a skills marketplace, subagents, online training, or multi-provider routing.

Saved trajectories now include provenance labels from the run. A trajectory can still be stored locally with low-trust influence for audit value, but `topagent memory trajectory review` and `topagent memory trajectory export` refuse artifacts that remain influenced by unresolved low-trust content.

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `topagent: command not found` | `source "$HOME/.cargo/env"` |
| `A C compiler is required` | `sudo apt install -y build-essential` |
| `API key required` | Set `--api-key` (OpenRouter) or `--opencode-api-key` (Opencode), or set `OPENROUTER_API_KEY` / `OPENCODE_API_KEY` |
| `Workspace path does not exist` | Run from a repo, pass `--workspace`, or run `topagent setup` |
| `Telegram bot token looks invalid` | Get a valid token from BotFather |
| `Telegram webhook is configured` | Remove the webhook, then retry |
| `systemd user services are unavailable` | Log into a desktop session where `systemctl --user` works |

## Current limitations

- Telegram: private chats only, text messages only
- One workspace per process
- Linux only (systemd required for background service)

## Verified delivery

Code-changing runs end with a structured delivery summary that explicitly surfaces verification status:

- **Verified runs**: Summary shows files touched, verification commands run, and pass/fail status with exit codes
- **Unverified runs**: Summary shows explicit "Not verified" status and reason (e.g., "no files changed" or "verification not attempted")
- **Failed verification**: Summary shows explicit failure status with the failing command and exit code
- **Analysis-only and no-op**: No delivery summary attached — output stays lightweight

Verification may be attempted as a bounded best-effort follow-through when files changed but no verification was run. The operator sees verification status explicitly rather than behind optimistic success wording.

## Documentation

- [Architecture](docs/architecture.md) -- crate structure, modules, runtime flows
- [Operations](docs/operations.md) -- install, service lifecycle, persistence, troubleshooting
- [Review Rules](REVIEW_RULES.md) -- short LLM preflight and post-change checks before meaningful code changes

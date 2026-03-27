# TopAgent

TopAgent is a local coding agent for a single workspace. It runs on your machine, uses OpenRouter for model access, and can answer either from the CLI or from a Telegram bot.

## Xubuntu Quick Start

This is the shortest path to a real end-to-end test on Xubuntu.

### 1. Install system dependencies

```bash
sudo apt update
sudo apt install -y build-essential curl git
```

### 2. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

### 3. Build and install TopAgent

```bash
git clone https://github.com/topway-ai/topagent.git
cd topagent
cargo install --path crates/topagent-cli
```

### 4. Export the required secrets

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
```

### 5. Pick the workspace TopAgent should operate in

```bash
export TOPAGENT_WORKSPACE="$HOME/path/to/your/repo"
```

TopAgent uses the directory you pass with `--workspace`. If you omit it, TopAgent uses the current directory.

### 6. Verify the CLI path first

```bash
topagent run --workspace "$TOPAGENT_WORKSPACE" "summarize this repository"
```

If startup is correct, TopAgent logs the provider, model, and workspace before it runs.

## Telegram Bot Quick Start

### 1. Create the bot

1. Open Telegram.
2. Message `@BotFather`.
3. Run `/newbot`.
4. Copy the bot token into `TELEGRAM_BOT_TOKEN`.

If this bot was previously used with a webhook, remove the webhook before using TopAgent long polling.

### 2. Start TopAgent in Telegram mode

```bash
topagent telegram serve --workspace "$TOPAGENT_WORKSPACE"
```

On startup you should see log lines showing:

- the workspace path
- the provider and model
- the bot username
- that TopAgent is using private text chats only

### 3. Perform the first real Telegram test

1. Open a private chat with your bot.
2. Send `/start`.
3. Confirm that the reply shows the same workspace path you passed with `--workspace`.
4. Send a real task such as:

```text
Summarize this repository and tell me which files are the main entry points.
```

## What Success Looks Like

- `topagent run` starts without asking you to guess missing configuration.
- `topagent telegram serve` starts and prints the workspace and bot identity.
- `/start` replies in Telegram and shows the workspace path.
- A plain text message in a private chat produces a real agent response.

## Common First-Run Failures

- `OpenRouter API key required: set --api-key or OPENROUTER_API_KEY`
  Set `OPENROUTER_API_KEY` or pass `--api-key` to `topagent run`.

- `Telegram bot token required: set --token or TELEGRAM_BOT_TOKEN`
  Set `TELEGRAM_BOT_TOKEN` or pass `--token` to `topagent telegram serve`.

- `Telegram bot token looks invalid`
  The token should look like `123456:ABCdef...`.

- `Workspace path does not exist`
  Fix the `--workspace` path or run TopAgent from inside the repo you want it to use.

- `Telegram webhook is configured`
  Remove the webhook first, then restart `topagent telegram serve`.

- `Failed to validate bot token (getMe failed)`
  The token is wrong, revoked, or Telegram is unreachable from this machine.

## Current Limitations

- Telegram mode supports private chats only.
- Telegram mode supports text messages only.
- Telegram chat history is stored in memory and resets on process restart.
- One TopAgent process uses one workspace at a time.

## Safety

TopAgent can read, write, edit, run bash commands, and perform git operations inside the chosen workspace. Point it only at directories you trust.

## Uninstall

```bash
cargo uninstall topagent-cli
```

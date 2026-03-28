# TopAgent

Install TopAgent on Xubuntu:

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Configure once:

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
export TOPAGENT_WORKSPACE="$HOME/path/to/your/repo"
```

First local run:

```bash
topagent "summarize this repository"
```

Telegram first test:

```bash
export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
topagent telegram
```

Then:

1. Open a private chat with your bot.
2. Send `/start` and confirm the workspace path is correct.
3. Send: `Summarize this repository and tell me the main entry points.`

If this fails:

- `topagent: command not found`
  Run `source "$HOME/.cargo/env"`

- `A C compiler is required`
  Run `sudo apt update && sudo apt install -y build-essential`

- `OpenRouter API key required`
  Export `OPENROUTER_API_KEY`

- `Workspace path does not exist`
  Fix `TOPAGENT_WORKSPACE` or pass `--workspace /path/to/repo`

- `Telegram bot token required` or `Telegram bot token looks invalid`
  Export a real `TELEGRAM_BOT_TOKEN` from BotFather

- `Telegram webhook is configured`
  Remove the webhook, then run `topagent telegram` again

Current limits:

- private chats only
- text messages only
- one workspace per process
- Telegram chat history resets on restart

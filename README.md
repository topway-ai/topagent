# TopAgent

Install TopAgent on Xubuntu:

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Configure once:

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
```

First local run:

```bash
cd /path/to/your/repo
topagent "summarize this repository"
```

Press `Ctrl-C` once to request a stop. Press it again to force exit.

Telegram background service:

```bash
cd /path/to/your/repo
export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
topagent service install
```

Then:

1. Open a private chat with your bot.
2. Send `/start` and confirm the workspace path is correct.
3. Send: `Summarize this repository and tell me the main entry points.`
4. Send `/stop` if you want to cancel the current task.

Inspect or remove the service:

```bash
topagent service status
topagent service uninstall
```

Foreground debugging:

```bash
cd /path/to/your/repo
export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
topagent telegram
```

If this fails:

- `topagent: command not found`
  Run `source "$HOME/.cargo/env"`

- `A C compiler is required`
  Run `sudo apt update && sudo apt install -y build-essential`

- `OpenRouter API key required`
  Export `OPENROUTER_API_KEY`

- `Workspace path does not exist`
  Run TopAgent from the repo you want to use, or pass `--workspace /path/to/repo`

- `Telegram bot token required` or `Telegram bot token looks invalid`
  Export a real `TELEGRAM_BOT_TOKEN` from BotFather

- `Telegram webhook is configured`
  Remove the webhook, then run `topagent telegram` again

- `systemd user services are unavailable`
  Log into a normal Linux desktop session where `systemctl --user` works, then run `topagent service install` again

Uninstall:

```bash
topagent uninstall
```

Removes the installed binary and stops any running TopAgent processes.
Does not remove your source repos, workspaces, or shell profile exports.

Current limits:

- private chats only
- text messages only
- one workspace per process
- Telegram chat history resets on restart

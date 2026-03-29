# TopAgent

Install TopAgent on Xubuntu:

```bash
curl -fsSL https://raw.githubusercontent.com/topway-ai/topagent/main/scripts/install.sh | bash
```

Telegram local setup:

```bash
topagent install
```

TopAgent will prompt for:

- your OpenRouter API key
- your Telegram bot token

It will then:

- create a default `workspace/` directory next to the installed binary, or in the repo root when running from source
- write a managed config/env file under `~/.config/topagent/`
- install and start the `topagent-telegram.service` user service
- persist Telegram chat history under `workspace/.topagent/telegram-history/`

Check health, manage the service, or remove the setup:

```bash
topagent status
topagent service start
topagent service stop
topagent service restart
topagent uninstall
```

Then:

1. Open a private chat with your bot.
2. Send `/start` and confirm the workspace path is correct.
3. Send: `Summarize this repository and tell me the main entry points.`
4. Send `/stop` if you want to cancel the current task.
5. Send `/reset` if you want to clear the saved conversation history for that chat.

First local one-shot run:

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
cd /path/to/your/repo
topagent "summarize this repository"
```

Press `Ctrl-C` once to request a stop. Press it again to force exit.

Foreground debugging:

```bash
export OPENROUTER_API_KEY="your_openrouter_key"
export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
topagent telegram --workspace /path/to/your/repo
```

Service notes:

- `topagent install` enables and starts the background Telegram service immediately.
- `topagent service restart` reloads the installed bot process without changing config.
- Chat history survives service restarts because it is stored in the configured workspace.
- `/reset` clears the persisted history for the current Telegram chat.

If this fails:

- `topagent: command not found`
  Run `source "$HOME/.cargo/env"`

- `A C compiler is required`
  Run `sudo apt update && sudo apt install -y build-essential`

- `OpenRouter API key required`
  Run `topagent install`, or export `OPENROUTER_API_KEY` for one-shot / foreground debugging

- `Workspace path does not exist`
  Run TopAgent from the repo you want to use, pass `--workspace /path/to/repo`, or let `topagent install` create the default workspace

- `Telegram bot token required` or `Telegram bot token looks invalid`
  Run `topagent install` and enter a real `TELEGRAM_BOT_TOKEN` from BotFather

- `Telegram webhook is configured`
  Remove the webhook, then run `topagent telegram` again

- `systemd user services are unavailable`
  Log into a normal Linux desktop session where `systemctl --user` works, then run `topagent install` again

Current limits:

- private chats only
- text messages only
- one workspace per process

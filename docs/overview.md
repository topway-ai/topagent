# Overview

## What TopAgent is

TopAgent is a local coding agent. You give it a task in natural language, and it reads your repository, makes a plan, and carries out the work using file operations and shell commands. It operates entirely within a workspace directory on your machine.

It uses an LLM (via OpenRouter) to decide what to do at each step. The agent loop runs locally -- the LLM sees your code and tool output, but the tools themselves execute on your machine.

## Usage modes

**One-shot CLI**: Run `topagent "your task"` from a repository. The agent works through the task and prints a final result. Suitable for single tasks like summarization, refactoring, or code review.

**Telegram bot**: Run `topagent install` to set up a background service. Send tasks to the bot from your phone or desktop. The bot keeps a thin workspace memory index plus a per-chat saved transcript, then retrieves only targeted evidence instead of replaying the whole transcript into model context.

**Foreground Telegram** (debugging): Run `topagent telegram` to start the bot in the foreground with log output visible in the terminal. Useful for debugging bot behavior.

## What it can do

- Read, write, and edit files in the workspace
- Run shell commands (sandboxed with bubblewrap when available)
- Use git: status, diff, branch, add, commit
- Plan multi-step tasks before executing them
- Load project-specific instructions from `TOPAGENT.md`
- Keep a tiny workspace memory index in `.topagent/MEMORY.md`
- Load compact topic notes from `.topagent/topics/` only when relevant
- Search prior Telegram transcripts as evidence without restoring them wholesale
- Create, repair, list, and remove workspace-local custom tools when the task explicitly asks for tool work
- Save reusable plans and lessons to `.topagent/`

## What it is not

- Not a hosted service. Everything runs on your machine.
- Not multi-user. One workspace per process, one bot token per service.
- Not an IDE plugin. It operates via CLI and Telegram, not editor integration.
- Not a general chatbot. It is designed for coding tasks within a repository.

## Design goals

1. **Local-first**: all tool execution happens on your machine, in your workspace
2. **Plan before act**: non-trivial tasks require a plan before the agent makes changes
3. **Verifiable output**: the agent reports what files changed and includes diff evidence
4. **Secret safety**: API keys and tokens are redacted from tool output and replies
5. **Minimal dependencies**: two Rust crates, OpenRouter API access, optional bubblewrap
6. **Index-first memory**: durable memory stays small, topic files are lazy, transcripts are evidence

## Agent behavior

For non-trivial tasks, the agent follows a Research -> Plan -> Build loop:

1. **Research**: reads files, checks git status, inspects the codebase
2. **Plan**: creates a step-by-step plan using the `update_plan` tool
3. **Build**: executes each step, updating plan status as it goes

The planning gate blocks mutation tools (write, edit, bash commands that modify files) until a plan exists. For simple one-step tasks, the agent skips planning.

The agent runs up to 50 steps by default (configurable with `--max-steps`). If it hits the limit, it reports what it accomplished.

## Tools available to the agent

| Tool | Purpose |
|------|---------|
| `read` | Read file contents (text only, truncated at 64KB) |
| `write` | Create or overwrite files |
| `edit` | Find-and-replace within files |
| `bash` | Run shell commands in the workspace |
| `git_status` | Check repository status |
| `git_diff` | View uncommitted changes |
| `git_branch` | Check or list branches |
| `git_add` | Stage files |
| `git_commit` | Create commits |
| `update_plan` | Create or update a task plan |
| `save_plan` | Archive a plan to `.topagent/plans/` |
| `save_lesson` | Save a lesson note to `.topagent/lessons/` |

Custom tools are stored in `.topagent/tools/`. Tool-authoring tools are only exposed when the task is explicitly about creating, repairing, listing, or deleting workspace tools. Broken generated tools are reported as workspace warnings.

## Current limitations

- **Telegram**: private chats only, text messages only (no images, files, or group chats)
- **Provider**: OpenRouter is the only supported LLM provider
- **Platform**: Linux only; systemd required for the background service
- **Workspace**: one workspace per process; the agent cannot operate across repositories
- **Network**: bash commands run with network disabled when bubblewrap is available
- **Context**: TopAgent no longer restores whole Telegram transcripts by default; it injects a small memory briefing and only targeted transcript snippets when relevant
- **Model**: default model is `minimax/minimax-m2.7`; quality depends on the model used

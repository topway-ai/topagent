# rust-pi

A minimal coding agent that runs locally in your workspace. It uses an LLM via OpenRouter to execute file operations, shell commands, and git actions within your project.

## Current Status

Early-phase project. Works for basic file operations, shell commands, git workflows, and task planning. Not production-tested. Use with caution.

## Prerequisites

- Rust (1.75+)
- OpenRouter API key

## Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

## Clone and Build

```bash
git clone https://github.com/topway-ai/rust-pi.git
cd rust-pi
cargo build --release
```

The binary is at `target/release/pi`.

## Quick Start

Run one command to test OpenRouter. Replace YOUR_API_KEY with your actual OpenRouter key:

```bash
./target/release/pi --api-key YOUR_API_KEY --cwd /path/to/your/project "summarize this project"
```

The agent uses MiniMax M2.7 by default.

## Example Commands

```bash
# Inspect a project
./target/release/pi --api-key YOUR_API_KEY --cwd /path/to/project "give me a summary"

# Check git status
./target/release/pi --api-key YOUR_API_KEY --cwd /path/to/project "show git status"

# Make a small edit
./target/release/pi --api-key YOUR_API_KEY --cwd /path/to/project "add a TODO comment to src/main.rs"
```

## Optional: Workspace Files

**PI.md** - Project instructions (optional, place in workspace root):

```markdown
# Project Instructions

- Use TypeScript, not JavaScript
- Run tests before committing
```

**commands.json** - Custom commands (optional):

```json
[
  {"name": "test", "description": "Run tests", "command": "cargo", "args_template": "test --all"}
]
```

## CLI Options

```bash
./target/release/pi --help

Options:
  --api-key KEY      OpenRouter API key
  --model MODEL      Model to use (default: minimax/minimax-m2.7)
  --cwd DIR          Working directory
  --max-steps N      Max steps (default: 50)
  --max-retries N    Retries (default: 3)
  --timeout-secs N   Timeout (default: 120)
```

## Safety

This agent can read/write/edit files, run shell commands, and execute git operations. Only run in directories you trust.
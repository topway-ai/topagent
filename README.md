# rust-pi

A minimal coding agent that runs locally in your workspace. It uses an LLM via OpenRouter to understand your instructions and executes file operations, shell commands, and git actions within your project.

## Current Status

Early-phase project. Works for basic file operations, shell commands, git workflows, and multi-step task planning. Not production-tested. Use with caution.

## Capabilities

- **File operations**: read, write, edit files
- **Shell execution**: run bash commands in workspace
- **Git workflow**: status, diff, branch, add, commit
- **Planning**: track multi-step tasks with update_plan tool

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

```bash
# Build the binary
cargo build --release

# Run with OpenRouter (replace YOUR_API_KEY with your actual key)
cd /path/to/your/project
/path/to/rust-pi/target/release/pi --api-key YOUR_API_KEY "summarize this project"
```

First run may take a moment. The agent will use the default MiniMax M2.7 model via OpenRouter.

## Example Commands

```bash
# Inspect a project
pi "give me a summary of this codebase"

# Check git status
pi "show me the current git status and any uncommitted changes"

# Make a small edit
pi "add a TODO comment to the main function in src/main.rs"

# Multi-step task with planning
pi "create a new file called FEATURES.md listing the main capabilities"
```

## Workspace Files

**PI.md** - Optional project instructions file. Place in workspace root:

```markdown
# Project Instructions

- Use TypeScript, not JavaScript
- Run tests before committing
```

**commands.json** - Optional custom commands. Place in workspace root:

```json
[
  {"name": "test", "description": "Run tests", "command": "cargo", "args_template": "test --all"},
  {"name": "lint", "description": "Run linter", "command": "cargo", "args_template": "clippy --fix"}
]
```

## CLI Options

```bash
pi --help

Options:
  --api-key KEY       OpenRouter API key
  --model MODEL       Model to use (default: minimax/minimax-m2.7)
  --cwd DIR           Working directory
  --max-steps N       Max agent steps (default: 50)
  --max-retries N     Provider retries (default: 3)
  --timeout-secs N    Provider timeout (default: 120)
```

## Safety Note

This is a trusted local agent. It can:
- Read/write/edit files in your workspace
- Execute shell commands
- Run git add/commit

Only run in directories you trust. Review instructions before execution.

## Limitations

- No file change confirmation before execution
- No built-in undo/revert
- Limited error recovery for complex operations
- No remote code execution protection beyond workspace bounds
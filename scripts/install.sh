#!/usr/bin/env bash
set -euo pipefail

TOPAGENT_GIT_URL="${TOPAGENT_INSTALL_GIT_URL:-https://github.com/topway-ai/topagent}"
TOPAGENT_GIT_BRANCH="${TOPAGENT_INSTALL_BRANCH:-main}"
TOPAGENT_INSTALL_PATH="${TOPAGENT_INSTALL_PATH:-}"
TOPAGENT_INSTALL_ROOT="${TOPAGENT_INSTALL_ROOT:-}"

say() {
  printf '==> %s\n' "$*"
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

ensure_compiler() {
  if have cc || have gcc || have clang; then
    return
  fi

  fail "A C compiler is required. On Xubuntu run: sudo apt update && sudo apt install -y build-essential"
}

ensure_cargo() {
  if have cargo; then
    return
  fi

  say "Rust was not found. Installing rustup and cargo."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  have cargo || fail "cargo is still unavailable. Run: source \"$HOME/.cargo/env\""
}

install_topagent() {
  local -a install_args
  install_args=(install --locked --force)

  if [[ -n "$TOPAGENT_INSTALL_ROOT" ]]; then
    install_args+=(--root "$TOPAGENT_INSTALL_ROOT")
  fi

  if [[ -n "$TOPAGENT_INSTALL_PATH" ]]; then
    if [[ ! -d "$TOPAGENT_INSTALL_PATH/crates/topagent-cli" ]]; then
      fail "TOPAGENT_INSTALL_PATH must point at the TopAgent repo root"
    fi
    install_args+=(--path "$TOPAGENT_INSTALL_PATH/crates/topagent-cli")
  else
    install_args+=(--git "$TOPAGENT_GIT_URL" --branch "$TOPAGENT_GIT_BRANCH" topagent-cli)
  fi

  cargo "${install_args[@]}"
}

main() {
  ensure_compiler
  ensure_cargo

  say "Installing TopAgent"
  install_topagent

  local installed_bin
  if [[ -n "$TOPAGENT_INSTALL_ROOT" ]]; then
    installed_bin="$TOPAGENT_INSTALL_ROOT/bin/topagent"
  else
    installed_bin="$HOME/.cargo/bin/topagent"
  fi

  cat <<EOF

TopAgent installed.

Next:
  export OPENROUTER_API_KEY="your_openrouter_key"
  export TOPAGENT_WORKSPACE="/path/to/your/repo"
  topagent "summarize this repository"

Telegram test:
  export TELEGRAM_BOT_TOKEN="123456:ABCdefYourBotToken"
  topagent telegram

If 'topagent' is not in your PATH yet, run:
  source "$HOME/.cargo/env"

Installed binary:
  $installed_bin
EOF
}

main "$@"

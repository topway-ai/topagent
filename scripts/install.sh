#!/usr/bin/env bash
set -euo pipefail

TOPAGENT_GIT_URL="${TOPAGENT_INSTALL_GIT_URL:-https://github.com/topway-ai/topagent}"
TOPAGENT_GIT_BRANCH="${TOPAGENT_INSTALL_BRANCH:-main}"
TOPAGENT_INSTALL_PATH="${TOPAGENT_INSTALL_PATH:-}"
TOPAGENT_INSTALL_ROOT="${TOPAGENT_INSTALL_ROOT:-}"
TOPAGENT_INSTALL_VERSION="${TOPAGENT_INSTALL_VERSION:-}"
TOPAGENT_INSTALL_RELEASE_BASE_URL="${TOPAGENT_INSTALL_RELEASE_BASE_URL:-https://github.com/topway-ai/topagent/releases}"
TOPAGENT_INSTALL_USE_CARGO="${TOPAGENT_INSTALL_USE_CARGO:-}"
TOPAGENT_SKIP_SETUP="${TOPAGENT_SKIP_SETUP:-}"

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

need_cmd() {
  have "$1" || fail "$2"
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

resolve_install_dir() {
  if [[ -n "$TOPAGENT_INSTALL_ROOT" ]]; then
    printf '%s\n' "$TOPAGENT_INSTALL_ROOT/bin"
  else
    printf '%s\n' "$HOME/.cargo/bin"
  fi
}

detect_release_target() {
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64)
      printf '%s\n' "x86_64-unknown-linux-gnu"
      ;;
    *)
      return 1
      ;;
  esac
}

release_download_prefix() {
  if [[ -n "$TOPAGENT_INSTALL_VERSION" ]]; then
    printf '%s/download/%s\n' "$TOPAGENT_INSTALL_RELEASE_BASE_URL" "$TOPAGENT_INSTALL_VERSION"
  else
    printf '%s/latest/download\n' "$TOPAGENT_INSTALL_RELEASE_BASE_URL"
  fi
}

install_topagent_from_cargo() {
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

install_topagent_from_release() {
  local install_dir installed_bin target asset_name checksum_name download_prefix temp_dir
  install_dir="$(resolve_install_dir)"
  installed_bin="$install_dir/topagent"
  target="$(detect_release_target)" || fail "No precompiled TopAgent asset is published for $(uname -s) $(uname -m). Re-run with TOPAGENT_INSTALL_USE_CARGO=1 to fall back to cargo install."
  asset_name="topagent-$target"
  checksum_name="$asset_name.sha256"
  download_prefix="$(release_download_prefix)"
  temp_dir="$(mktemp -d)"

  need_cmd curl "curl is required to download the TopAgent release asset."
  need_cmd sha256sum "sha256sum is required to verify the TopAgent release asset."
  need_cmd install "install is required to place the TopAgent binary."

  mkdir -p "$install_dir"

  curl -fsSL "$download_prefix/$asset_name" -o "$temp_dir/$asset_name" || fail "Failed to download TopAgent release asset from $download_prefix/$asset_name"
  curl -fsSL "$download_prefix/$checksum_name" -o "$temp_dir/$checksum_name" || fail "Failed to download TopAgent release checksum from $download_prefix/$checksum_name"

  (
    cd "$temp_dir"
    sha256sum -c "$checksum_name"
  ) || fail "Checksum verification failed for TopAgent release asset."

  install -m 0755 "$temp_dir/$asset_name" "$installed_bin"
  rm -rf "$temp_dir"
}

run_post_install_setup() {
  local installed_bin="$1"

  if [[ -n "$TOPAGENT_SKIP_SETUP" ]]; then
    return
  fi

  if [[ ! -r /dev/tty || ! -w /dev/tty ]]; then
    return
  fi

  say "Starting interactive TopAgent setup"
  if "$installed_bin" install </dev/tty >/dev/tty 2>/dev/tty; then
    return
  fi

  say "TopAgent was installed, but interactive setup did not complete. Run:"
  printf '  %s install\n' "$installed_bin"
}

main() {
  say "Installing TopAgent"

  local install_dir installed_bin
  install_dir="$(resolve_install_dir)"
  installed_bin="$install_dir/topagent"

  if [[ -n "$TOPAGENT_INSTALL_PATH" || -n "$TOPAGENT_INSTALL_USE_CARGO" ]]; then
    ensure_compiler
    ensure_cargo
    install_topagent_from_cargo
  else
    install_topagent_from_release
  fi

  run_post_install_setup "$installed_bin"

  cat <<EOF

TopAgent installed.

Set up the Telegram background service:
  $installed_bin install

Check service health:
  $installed_bin status

Foreground one-shot run:
  cd /path/to/your/repo
  export OPENROUTER_API_KEY="your_openrouter_key"
  $installed_bin "summarize this repository"

Installed binary:
  $installed_bin
EOF

  if [[ -z "$TOPAGENT_INSTALL_ROOT" ]]; then
    cat <<EOF

If 'topagent' is not in your PATH yet, run:
  source "$HOME/.cargo/env"
EOF
  else
    cat <<EOF

If 'topagent' is not in your PATH yet, add this directory:
  $install_dir
EOF
  fi
}

main "$@"

#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --locked

if [[ "${1:-}" == "--release-binary" ]]; then
  cargo build --locked --release -p topagent-cli --bin topagent
else
  cargo build --locked
fi

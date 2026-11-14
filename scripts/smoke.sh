#!/usr/bin/env bash
# Stranger smoke test: build and exercise heavy-slot and agent-mail from a clean tree.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/smoke}"
export RUST_BACKTRACE="${RUST_BACKTRACE:-0}"

echo "== smoke: cargo build (heavy-slot, agent-mail) =="
cargo build --release -p heavy-slot -p agent-mail

HS="$CARGO_TARGET_DIR/release/heavy-slot"
AM="$CARGO_TARGET_DIR/release/agent-mail"
test -x "$HS"
test -x "$AM"

echo "== smoke: crate tests =="
cargo test -p heavy-slot -p agent-mail

echo "== smoke: heavy-slot -- true =="
"$HS" -- true

STATE="$(mktemp -d)"
CFG="$(mktemp -d)"
export AGENT_MAIL_STATE="$STATE"
export AGENT_MAIL_CONFIG="$CFG"
"$AM" install smokescreen >/dev/null
trap 'rm -rf "$STATE" "$CFG"' EXIT

echo "== smoke: agent-mail send/unread =="
set +e
ID=$("$AM" send local smoke-ping)
set -e
UNREAD=$("$AM" unread --json)
case "$UNREAD" in *"$ID"*) ;; *) echo "smoke: unread json missing id $ID" >&2; exit 1 ;; esac
"$AM" unread --mark --json >/dev/null
AFTER=$("$AM" unread --json)
case "$AFTER" in *'"unread":[]'*) ;; *) echo "smoke: expected empty unread list, got: $AFTER" >&2; exit 1 ;; esac

echo "smoke: OK"

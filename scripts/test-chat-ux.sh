#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

if [ -n "${AGENTNOISE_BIN:-}" ]; then
  bin="$AGENTNOISE_BIN"
elif [ -x target/debug/agentnoise ]; then
  cargo build >/dev/null
  bin="target/debug/agentnoise"
else
  bin="target/debug/agentnoise"
  cargo build
fi

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT INT TERM

config="$tmpdir/config.toml"
"$bin" --config "$config" init --force --direct-agents >/dev/null
perl -0pi -e 's#data_dir = ".*?"#data_dir = "'"$tmpdir/data"'"#s; s#log_dir = ".*?"#log_dir = "'"$tmpdir/logs"'"#s; s#worktree_dir = ".*?"#worktree_dir = "'"$tmpdir/worktrees"'"#s; s#allowed_senders = \\[\\]#allowed_senders = [\n    "phone",\n]#s' "$config"

last_out="$tmpdir/last.out"

send_msg() {
  group="$1"
  message="$2"
  "$bin" --config "$config" handle --group "$group" --sender phone "$message" >"$last_out"
  printf '\n>>> %s %s\n' "$group" "$message"
  sed -n '1,80p' "$last_out"
}

expect() {
  pattern="$1"
  if ! grep -Fq "$pattern" "$last_out"; then
    echo "missing expected text: $pattern" >&2
    exit 1
  fi
}

send_msg alpha111111 "/status"
expect "Status: OK"
expect "Sessions:"

send_msg alpha111111 "/rename main"
expect "Session: main"
expect "Workspace: sandbox:/"

send_msg alpha111111 "/cd src"
expect "Workspace: sandbox:/src"

send_msg alpha111111 "/list"
expect "main (current)"
expect "workspace: sandbox:/src"
expect "Send /jump <number|name|id>."

send_msg alpha111111 "/new bugfix ui"
expect "Created session: bugfix-ui"
expect "I opened a new White Noise chat named \"agentnoise: bugfix-ui\""
expect "This chat is ready."

send_msg beta222222 "/rename bugfix"
expect "Session: bugfix"
expect "Workspace: sandbox:/"

send_msg alpha111111 "/list"
expect "1. bugfix"
expect "2. main (current)"
expect "chat: beta2"
expect "chat: alpha"

send_msg alpha111111 "/resume bugfix"
expect "Resumed session: bugfix"
expect "chat id:beta2"
expect "Continue in that chat"
expect "Session: bugfix"
expect "Resumed here"

send_msg beta222222 "/close"
expect "Closed session: bugfix"
expect "/jump bugfix"

send_msg alpha111111 "/list"
expect "bugfix (closed)"
expect "main (current)"

if [ "${AGENTNOISE_CHAT_UX_FRONTIER:-0}" = "1" ]; then
  send_msg alpha111111 "/codex Reply with exactly: agentnoise-chat-ux-frontier-ok"
  expect "agentnoise-chat-ux-frontier-ok"
fi

echo
echo "chat UX smoke passed"

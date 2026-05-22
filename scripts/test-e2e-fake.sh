#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

echo "synthetic fake-phone smoke: embedded Darkmatter dual-runtime roundtrips" >&2

timeout_seconds="${AGENTNOISE_FAKE_PHONE_TIMEOUT:-120}"
if [ -n "${AGENTNOISE_BIN:-}" ]; then
  bin="$AGENTNOISE_BIN"
  if [ ! -x "$bin" ]; then
    echo "AGENTNOISE_BIN is not executable: $bin" >&2
    exit 1
  fi
else
  cargo build
  bin="target/debug/agentnoise"
fi

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT INT TERM

config="$tmpdir/config.toml"
"$bin" --config "$config" init --force --direct-agents >/dev/null
perl -0pi -e 's#data_dir = ".*?"#data_dir = "'"$tmpdir/data"'"#s; s#log_dir = ".*?"#log_dir = "'"$tmpdir/logs"'"#s; s#worktree_dir = ".*?"#worktree_dir = "'"$tmpdir/worktrees"'"#s' "$config"

run_roundtrip() {
  name="$1"
  shift
  out="$tmpdir/$name.out"
  err="$tmpdir/$name.err"
  if ! "$bin" --config "$config" fake-phone roundtrip \
    --timeout-seconds "$timeout_seconds" \
    --root "$tmpdir/$name-root" \
    "$@" >"$out" 2>"$err"; then
    sed -n '1,240p' "$out" >&2 || true
    sed -n '1,240p' "$err" >&2 || true
    echo "fake-phone $name roundtrip failed" >&2
    exit 1
  fi
  if grep -q "replies: none before timeout" "$out"; then
    sed -n '1,240p' "$out" >&2
    sed -n '1,240p' "$err" >&2 || true
    echo "fake-phone $name did not receive a reply" >&2
    exit 1
  fi
}

run_roundtrip status --expect "received: /status" /status
run_roundtrip help --expect "commands: /help /status /codex" /help

codex_phrase="agentnoise-fake-phone-e2e-ok"
run_roundtrip codex \
  --require-job-final \
  --expect "codex queued: Reply with exactly: $codex_phrase" \
  /codex "Reply with exactly: $codex_phrase"

echo "fake-phone synthetic roundtrip passed"

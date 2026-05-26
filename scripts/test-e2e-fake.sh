#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

echo "live fake-phone smoke: starts isolated darkmatter transport against a mock relay" >&2

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
  status=$?
  if [ "$status" -eq 0 ] && [ -z "${AGENTNOISE_KEEP_FAKE_PHONE_TMP:-}" ]; then
    rm -rf "$tmpdir"
  else
    echo "fake-phone temp kept at $tmpdir" >&2
  fi
}
trap cleanup EXIT INT TERM

config="$tmpdir/base-config.toml"
"$bin" --config "$config" init --force --direct-agents >/dev/null

if ! "$bin" --config "$config" fake-phone live-roundtrip \
  --root "$tmpdir/status" \
  --timeout-seconds "$timeout_seconds" \
  --expect "running" \
  /status >"$tmpdir/status.out" 2>"$tmpdir/status.err"; then
  sed -n '1,220p' "$tmpdir/status.out" >&2 || true
  sed -n '1,220p' "$tmpdir/status.err" >&2 || true
  echo "live fake-phone /status roundtrip failed" >&2
  exit 1
fi

if ! grep -q "journal: inbound=true outbound=true" "$tmpdir/status.out"; then
  sed -n '1,220p' "$tmpdir/status.out" >&2
  echo "live fake-phone /status did not journal both directions" >&2
  exit 1
fi

if ! "$bin" --config "$config" fake-phone live-roundtrip \
  --root "$tmpdir/help" \
  --timeout-seconds "$timeout_seconds" \
  --expect "/status" \
  /help >"$tmpdir/help.out" 2>"$tmpdir/help.err"; then
  sed -n '1,220p' "$tmpdir/help.out" >&2 || true
  sed -n '1,220p' "$tmpdir/help.err" >&2 || true
  echo "live fake-phone /help roundtrip failed" >&2
  exit 1
fi

if ! "$bin" --config "$config" fake-phone live-roundtrip \
  --root "$tmpdir/codex-worker" \
  --timeout-seconds "$timeout_seconds" \
  --start-worker \
  --min-replies 2 \
  --expect "codex queued" \
  --expect "agentnoise-darkmatter-live-ok" \
  --require-job-final \
  /codex "Reply with exactly: agentnoise-darkmatter-live-ok" \
  >"$tmpdir/codex.out" 2>"$tmpdir/codex.err"; then
  sed -n '1,260p' "$tmpdir/codex.out" >&2 || true
  sed -n '1,260p' "$tmpdir/codex.err" >&2 || true
  echo "live fake-phone /codex worker roundtrip failed" >&2
  exit 1
fi

echo "live fake-phone roundtrip passed"

#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

echo "live fake-phone smoke: run once on a workstation; do not loop this script on the primary machine" >&2

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
listener_pid=""
desktop_wnd_pid=""
cleanup() {
  if [ -n "$listener_pid" ]; then
    kill "$listener_pid" 2>/dev/null || true
    wait "$listener_pid" 2>/dev/null || true
  fi
  if [ -n "$desktop_wnd_pid" ]; then
    kill "$desktop_wnd_pid" 2>/dev/null || true
    wait "$desktop_wnd_pid" 2>/dev/null || true
  fi
  rm -rf "$tmpdir"
}
trap cleanup EXIT INT TERM

config="$tmpdir/config.toml"
"$bin" --config "$config" init --force >/dev/null
desktop_data="$tmpdir/desktop-wnd"
desktop_logs="$tmpdir/desktop-wnd-logs"
desktop_socket="$desktop_data/release/wnd.sock"
perl -0pi -e 's#wn_bin = "wn"#wn_bin = "wn"\nsocket = "'"$desktop_socket"'"#s; s#data_dir = ".*?"#data_dir = "'"$tmpdir/data"'"#s; s#log_dir = ".*?"#log_dir = "'"$tmpdir/logs"'"#s; s#worktree_dir = ".*?"#worktree_dir = "'"$tmpdir/worktrees"'"#s' "$config"
perl -0pi -e 's#pairing_pin_seconds = [0-9]+#pairing_pin_seconds = 600#' "$config"

wn_path="$("$bin" --config "$config" whitenoise path)"
wnd_bin="$(dirname "$wn_path")/wnd"
if [ ! -x "$wnd_bin" ]; then
  wnd_bin="$(command -v wnd || true)"
fi
if [ -z "$wnd_bin" ] || [ ! -x "$wnd_bin" ]; then
  echo "could not find wnd next to $wn_path or on PATH" >&2
  exit 1
fi

mkdir -p "$desktop_data" "$desktop_logs"
relays="wss://index.hzrd149.com,wss://indexer.coracle.social,wss://relay.primal.net,wss://relay.damus.io,wss://relay.ditto.pub,wss://nos.lol"
"$wnd_bin" \
  --data-dir "$desktop_data" \
  --logs-dir "$desktop_logs" \
  --discovery-relays "$relays" \
  >"$tmpdir/desktop-wnd.out" 2>"$tmpdir/desktop-wnd.err" &
desktop_wnd_pid="$!"

deadline=$(( $(date +%s) + 30 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  if ! kill -0 "$desktop_wnd_pid" 2>/dev/null; then
    sed -n '1,200p' "$tmpdir/desktop-wnd.out" >&2 || true
    sed -n '1,200p' "$tmpdir/desktop-wnd.err" >&2 || true
    echo "desktop wnd exited before its socket appeared" >&2
    exit 1
  fi
  if [ -S "$desktop_socket" ] || [ -e "$desktop_socket" ]; then
    break
  fi
  sleep 1
done
if [ ! -e "$desktop_socket" ]; then
  sed -n '1,200p' "$tmpdir/desktop-wnd.out" >&2 || true
  sed -n '1,200p' "$tmpdir/desktop-wnd.err" >&2 || true
  echo "timed out waiting for desktop wnd socket: $desktop_socket" >&2
  exit 1
fi

"$bin" --config "$config" up --dev-burner-nsec --ssh --no-daemon >"$tmpdir/listener.out" 2>"$tmpdir/listener.err" &
listener_pid="$!"

pin=""
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  if ! kill -0 "$listener_pid" 2>/dev/null; then
    sed -n '1,200p' "$tmpdir/listener.out" >&2 || true
    sed -n '1,200p' "$tmpdir/listener.err" >&2 || true
    echo "agentnoise listener exited before printing a pairing PIN" >&2
    exit 1
  fi
  pin_line="$(sed -n 's/^pairing PIN: \([0-9][0-9]*\) (expires in \([0-9][0-9]*\)s).*/\1 \2/p' "$tmpdir/listener.out" | tail -n 1)"
  pin="$(printf '%s\n' "$pin_line" | awk '$2 >= 90 { print $1 }')"
  if [ -n "$pin" ]; then
    break
  fi
  sleep 1
done

if [ -z "$pin" ]; then
  sed -n '1,200p' "$tmpdir/listener.out" >&2 || true
  sed -n '1,200p' "$tmpdir/listener.err" >&2 || true
  echo "timed out waiting for pairing PIN" >&2
  exit 1
fi

"$bin" --config "$config" fake-phone roundtrip \
  --root "$tmpdir/fake-phone" \
  --pin "$pin" \
  --timeout-seconds "$timeout_seconds" \
  /status >"$tmpdir/status.out" 2>"$tmpdir/status.err"

if grep -q "replies: none before timeout" "$tmpdir/status.out"; then
  sed -n '1,200p' "$tmpdir/status.out" >&2
  sed -n '1,200p' "$tmpdir/status.err" >&2 || true
  echo "fake-phone /status did not receive a reply" >&2
  exit 1
fi
if ! grep -q "Status: OK" "$tmpdir/status.out"; then
  sed -n '1,200p' "$tmpdir/status.out" >&2
  sed -n '1,200p' "$tmpdir/status.err" >&2 || true
  echo "fake-phone /status did not receive the agentnoise status reply" >&2
  exit 1
fi

"$bin" --config "$config" fake-phone roundtrip \
  --root "$tmpdir/fake-phone" \
  --timeout-seconds "$timeout_seconds" \
  /help >"$tmpdir/help.out" 2>"$tmpdir/help.err"

if grep -q "replies: none before timeout" "$tmpdir/help.out"; then
  sed -n '1,200p' "$tmpdir/help.out" >&2
  sed -n '1,200p' "$tmpdir/help.err" >&2 || true
  echo "fake-phone /help did not receive a reply" >&2
  exit 1
fi
if ! grep -q "/status" "$tmpdir/help.out"; then
  sed -n '1,200p' "$tmpdir/help.out" >&2
  sed -n '1,200p' "$tmpdir/help.err" >&2 || true
  echo "fake-phone /help did not receive the command list" >&2
  exit 1
fi

echo "fake-phone roundtrip passed"

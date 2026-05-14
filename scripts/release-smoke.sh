#!/bin/sh
set -eu

cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
config="$tmpdir/config.toml"

target/release/agentnoise --version
target/release/agentnoise config print-template >/dev/null
target/release/agentnoise --config "$config" init --force >/dev/null
perl -0pi -e 's#data_dir = ".*?"#data_dir = "'"$tmpdir/data"'"#s; s#log_dir = ".*?"#log_dir = "'"$tmpdir/logs"'"#s; s#worktree_dir = ".*?"#worktree_dir = "'"$tmpdir/worktrees"'"#s' "$config"
target/release/agentnoise --config "$config" status >/dev/null
target/release/agentnoise --config "$config" agents >/dev/null
target/release/agentnoise --config "$config" fake-phone plan >/dev/null

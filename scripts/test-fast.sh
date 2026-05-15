#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test

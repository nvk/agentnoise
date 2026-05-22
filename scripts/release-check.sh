#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/test-fast.sh
./scripts/test-fixtures.sh
./scripts/test-chat-ux.sh
cargo package --allow-dirty --list >/dev/null
cargo build --release --locked --offline
! rg -n "REPLACE_WITH_" packaging/homebrew/agentnoise.rb

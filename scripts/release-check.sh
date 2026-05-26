#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/test-fast.sh
./scripts/test-fixtures.sh
./scripts/test-chat-ux.sh
if rg -q 'marmot-protocol/darkmatter.git' Cargo.toml; then
  cargo build --release --locked
else
  cargo package --allow-dirty --offline
fi
! rg -n "REPLACE_WITH_" packaging/homebrew/agentnoise.rb

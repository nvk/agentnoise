#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/test-fast.sh
./scripts/test-fixtures.sh
cargo package --allow-dirty --offline
! rg -n "REPLACE_WITH_" packaging/homebrew/agentnoise.rb

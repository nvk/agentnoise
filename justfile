set dotenv-load := false

fmt:
    cargo fmt -- --check

check:
    cargo check

clippy:
    cargo clippy -- -D warnings

test:
    cargo test

verify: fmt check clippy test

release-check: verify
    cargo package --allow-dirty --offline
    ! rg -n "REPLACE_WITH_" packaging/homebrew/agentnoise.rb

release:
    cargo build --release

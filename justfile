set dotenv-load := false

fmt:
    cargo fmt -- --check

check:
    cargo check

clippy:
    cargo clippy -- -D warnings

test:
    cargo test

test-fast:
    ./scripts/test-fast.sh

test-fixtures:
    ./scripts/test-fixtures.sh

test-e2e-fake:
    ./scripts/test-e2e-fake.sh

verify: fmt check clippy test

release-check:
    ./scripts/release-check.sh

release:
    cargo build --release

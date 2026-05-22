set dotenv-load := false

dev_instance := "dev"
dev_log_filter := "agentnoise=debug,marmot_app::agent_stream=debug,transport_quic_broker=debug,transport_quic_stream=debug"

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

# Build (release) then run an isolated local-dev listener with debug logs.
# Dev instances use direct agents so a clean reset does not require bondage.
up instance=dev_instance: release
    AGENTNOISE_LOG="${AGENTNOISE_LOG:-{{ dev_log_filter }}}" ./target/release/agentnoise --instance "{{ instance }}" up --direct-agents

# Build (release) then run an isolated local-dev listener quietly.
# Dev instances use direct agents so a clean reset does not require bondage.
up-quiet instance=dev_instance: release
    ./target/release/agentnoise --instance "{{ instance }}" up --direct-agents

# Build (release) then run the default instance from this checkout.
# Stop the packaged/default service first, or this will contend for its lock.
up-default: release
    AGENTNOISE_LOG="${AGENTNOISE_LOG:-{{ dev_log_filter }}}" ./target/release/agentnoise up

# Build (release) then run the default instance quietly from this checkout.
up-default-quiet: release
    ./target/release/agentnoise up

# DESTRUCTIVE (macOS): wipe one named local-dev instance.
reset-dev instance=dev_instance:
    #!/usr/bin/env bash
    set -euo pipefail
    instance="{{ instance }}"
    data_root="$HOME/Library/Application Support/agentnoise/instances/$instance"
    log_dir="$HOME/Library/Logs/agentnoise/instances/$instance"
    echo "agentnoise reset-dev: stopping local instance '$instance'…"
    pkill -f "target/release/agentnoise --instance $instance" 2>/dev/null || true
    pkill -f "target/release/agentnoise .*--instance $instance" 2>/dev/null || true
    sleep 1
    echo "agentnoise reset-dev: clearing OS keychain items (service=agentnoise-$instance)…"
    while security delete-generic-password -s "agentnoise-$instance" >/dev/null 2>&1; do :; done
    echo "agentnoise reset-dev: removing data root → $data_root"
    rm -rf "$data_root"
    echo "agentnoise reset-dev: removing log dir   → $log_dir"
    rm -rf "$log_dir"
    echo "agentnoise reset-dev: done — next \`just up {{ instance }}\` starts that instance cleanly with --direct-agents."

# Wipe one named local-dev instance, then build + start it fresh.
fresh-dev instance=dev_instance: (reset-dev instance) (up instance)

# DESTRUCTIVE (macOS): wipe ALL local agentnoise state for a clean first-run
reset:
    #!/usr/bin/env bash
    # Stops any listener, clears the OS keychain secrets (service "agentnoise"),
    # and removes the data + log dirs (config, the darkmatter account home,
    # jobs/chat/approval state, runtime events).
    set -euo pipefail
    data_dir="$HOME/Library/Application Support/agentnoise"
    log_dir="$HOME/Library/Logs/agentnoise"
    echo "agentnoise reset: stopping any running listener…"
    pkill -f 'target/release/agentnoise' 2>/dev/null || true
    sleep 1
    echo "agentnoise reset: clearing OS keychain items (service=agentnoise)…"
    while security delete-generic-password -s agentnoise >/dev/null 2>&1; do :; done
    echo "agentnoise reset: removing data dir → $data_dir"
    rm -rf "$data_dir"
    echo "agentnoise reset: removing log dir  → $log_dir"
    rm -rf "$log_dir"
    echo "agentnoise reset: done — next \`just up\` starts the dev instance from a clean slate with --direct-agents."

# Wipe the default instance, then build + start the default instance fresh.
fresh: reset up-default

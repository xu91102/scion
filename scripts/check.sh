#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
target/debug/scion evaluate examples/ready.json
set +e
target/debug/scion evaluate examples/blocked.json
result=$?
set -e
test "$result" -eq 3
python3 scripts/check-workspaces.py target/debug/scion

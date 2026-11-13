#!/bin/sh
# Verify the distributable and a separate path consumer without touching hardware.
set -eu
cargo package --locked --allow-dirty
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
tar -xzf "${CARGO_TARGET_DIR:-target}/package/icc-ptp-safe-0.1.0.crate" -C "$tmp"
mkdir "$tmp/consumer"
cp tests/consumer/Cargo.toml.fixture "$tmp/consumer/Cargo.toml"
mkdir "$tmp/consumer/src"
cp tests/consumer/main.rs "$tmp/consumer/src/main.rs"
cargo test --locked --manifest-path "$tmp/icc-ptp-safe-0.1.0/Cargo.toml" --all-features
cargo build --locked --manifest-path "$tmp/icc-ptp-safe-0.1.0/Cargo.toml" --examples
cargo run --offline --manifest-path "$tmp/consumer/Cargo.toml"

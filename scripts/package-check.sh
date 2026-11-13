#!/bin/sh
# Verify the distributable and a separate path consumer without touching hardware.
set -eu
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
pkg="image-capture-$version"
cargo package --locked --allow-dirty
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
tar -xzf "${CARGO_TARGET_DIR:-target}/package/$pkg.crate" -C "$tmp"
mkdir "$tmp/consumer"
sed "s/image-capture-[0-9][0-9A-Za-z.+-]*/$pkg/" tests/consumer/Cargo.toml.fixture > "$tmp/consumer/Cargo.toml"
mkdir "$tmp/consumer/src"
cp tests/consumer/main.rs "$tmp/consumer/src/main.rs"
cargo test --locked --manifest-path "$tmp/$pkg/Cargo.toml" --all-features
cargo build --locked --manifest-path "$tmp/$pkg/Cargo.toml" --examples
cargo run --offline --manifest-path "$tmp/consumer/Cargo.toml"

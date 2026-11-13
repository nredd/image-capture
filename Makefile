.PHONY: fmt lint check test doc deny package examples all

export CARGO_BUILD_JOBS ?= 2
export MACOSX_DEPLOYMENT_TARGET ?= 13.0

fmt:
	cargo fmt --all -- --check
	rustfmt --edition 2024 --check tests/consumer/main.rs

lint:
	cargo clippy --locked --all-targets --all-features -- -D warnings

check:
	cargo check --locked --all-targets --all-features

test:
	cargo test --locked --all-features

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features

deny:
	cargo deny --locked check

package:
	sh scripts/package-check.sh

examples:
	cargo build --locked --examples
	cargo run --locked --example device_info

all: fmt lint check test doc deny examples package

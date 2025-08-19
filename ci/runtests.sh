#!/bin/bash

set -v
set -e

export CARGO_TARGET_DIR=target/ci

rustup target add thumbv7em-none-eabihf
rustup component add rustfmt clippy

export RUSTDOCFLAGS='-D warnings'
export RUSTFLAGS="-D warnings"

cargo fmt -- --check

# Check everything first
cargo check --locked
cargo clippy

# various features
cargo build --release
cargo build --release --all-features
cargo build --release --no-default-features
cargo build --release --features mctp-bench

(cd xspiloader && cargo build)

# Check syntax
cargo doc

# Record sizes
readelf -S "$CARGO_TARGET_DIR"/thumbv7em-none-eabihf/release/usbnvme

echo success

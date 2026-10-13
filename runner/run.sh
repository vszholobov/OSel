#!/bin/sh
# Cargo runner for the kernel. A script, so that cargo resolves it relative to the
# repository and `cargo run` works from any subdirectory.
exec cargo run --locked --quiet --manifest-path "$(dirname "$0")/Cargo.toml" --target host-tuple -- "$@"

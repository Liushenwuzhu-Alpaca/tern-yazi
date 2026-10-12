#!/bin/sh
# Run the real UI interaction suite in an isolated Tern window and daemon.
set -eu

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec cargo test --locked --manifest-path "$REPO/launcher/Cargo.toml" --test interaction -- --test-threads=1 --nocapture

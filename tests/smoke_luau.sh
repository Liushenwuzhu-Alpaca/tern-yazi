#!/bin/sh
# Run the real native UI interaction suite in an isolated Tern window and daemon.
set -eu

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec cargo test --locked --manifest-path "$REPO/launcher/Cargo.toml" --test interaction -- --nocapture

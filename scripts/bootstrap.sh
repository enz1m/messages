#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Cargo.lock owns the SDK and its public foundation revisions.
cargo +1.99.1 fetch --locked

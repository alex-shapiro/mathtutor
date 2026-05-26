#!/usr/bin/env bash
# Install `mt` from this checkout into ~/.cargo/bin (release build).

set -euo pipefail

cd "$(dirname "$0")"

cargo install --path . --locked

echo "Installed: $(command -v mt)"

#!/usr/bin/env bash
# Install `mt` from this checkout into ~/.cargo/bin (release build).

set -euo pipefail

cd "$(dirname "$0")"

cargo install --path . --locked

echo
echo "Installed: $(command -v mt)"
echo
echo "To run mt from outside this directory, set MT_GRAPH:"
echo
echo "    export MT_GRAPH=$(pwd)/curriculum/graph"
echo
echo "See AGENTS.md for usage."

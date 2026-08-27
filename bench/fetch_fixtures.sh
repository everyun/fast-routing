#!/bin/bash
# Fetch the upstream Freerouting fixture suite (the benchmark/test corpus).
# Result: /tmp/freerouting-upstream/fixtures/*.dsn  (~68 MB, 46 top-level boards)
# Tests and bench default to this location; override with FR_FIXTURES_DIR.
set -euo pipefail
DEST=${1:-/tmp/freerouting-upstream}
if [ -d "$DEST/fixtures" ] && [ -n "$(ls "$DEST"/fixtures/*.dsn 2>/dev/null)" ]; then
  echo "fixtures already present: $DEST/fixtures ($(ls "$DEST"/fixtures/*.dsn | wc -l | tr -d ' ') dsn files)"
  exit 0
fi
git clone --depth 1 --filter=blob:none --no-checkout \
  https://github.com/freerouting/freerouting "$DEST"
cd "$DEST"
git sparse-checkout set fixtures
git checkout HEAD -- fixtures
echo "fetched $(ls fixtures/*.dsn | wc -l | tr -d ' ') dsn files into $DEST/fixtures"

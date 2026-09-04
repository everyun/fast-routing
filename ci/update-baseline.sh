#!/usr/bin/env bash
# Refresh bench/ci-baseline.json unrouted_max from a smoke run of the 6 CI boards.
# Quality (added_violations == 0) still has to pass; time is recorded, not gated.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
exec python3 bench/tools/ci_smoke.py --update-baseline "$@"

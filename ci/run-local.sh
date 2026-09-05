#!/usr/bin/env bash
# Local (and GHA) CI: every step, in order, non-zero on first failure.
#
#   build + clippy + fmt + tests + fixture fetch + jar audit
#   + 6-board rust smoke (referee, regression ceilings)
#   + spec-constant lint over docs/upstream-study
#
# Does not modify crates/. Times each step for the report.
#
# Long tests (#[ignore]d, e.g. fr-router/tests/portfolio_race.rs — minutes each
# in a debug build) are skipped by default. `ci/run-local.sh --long` or
# FR_LONG_TESTS=1 runs them via `--include-ignored` after the normal suite.
set -euo pipefail

LONG=0
for arg in "$@"; do
  case "$arg" in
    --long) LONG=1 ;;
    *) echo "unknown argument: $arg (only --long is supported)" >&2; exit 2 ;;
  esac
done
if [ "${FR_LONG_TESTS:-0}" = "1" ]; then
  LONG=1
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

export CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-always}

log() { printf '%s\n' "$*"; }

step() {
  local name="$1"
  shift
  local start end elapsed
  start=$(date +%s)
  log ""
  log "======== ${name} ========"
  "$@"
  end=$(date +%s)
  elapsed=$((end - start))
  log "-------- ${name} done in ${elapsed}s --------"
  printf '%s\t%s\n' "$elapsed" "$name" >>"$TIMING"
}

TIMING=$(mktemp)
trap 'rm -f "$TIMING"' EXIT

log "repo  $ROOT"
log "rev   $(git rev-parse --short HEAD) $(git rev-parse --abbrev-ref HEAD)"
log "date  $(date -u +%Y-%m-%dT%H:%M:%SZ)"

step "yaml-parse workflows" \
  python3 -c 'import yaml,sys; [yaml.safe_load(open(f)) for f in sys.argv[1:]]; print("workflows ok")' \
    .github/workflows/ci.yml .github/workflows/release.yml

if command -v actionlint >/dev/null 2>&1; then
  step "actionlint" actionlint .github/workflows/ci.yml .github/workflows/release.yml
else
  log "actionlint not installed — skipped"
fi

# Fixture-dependent Rust tests fail loudly when the corpus is missing (G1.3).
step "fixture fetch" bash bench/fetch_fixtures.sh
step "jar fetch" bash ci/fetch-jar.sh

step "python bench self-tests" python3 bench/test_bench.py
step "python tools unittests" python3 -m unittest discover -s bench/tools/tests -v
step "jar audit" python3 bench/tools/jar_audit.py
step "spec lint" python3 bench/tools/spec_lint.py

# The workspace is rustfmt-clean and clippy-clean since the format task; both
# steps fail the pipeline on any drift.
step "fmt" cargo fmt --all -- --check

step "clippy" cargo clippy --workspace --all-targets -- -D warnings

# Run full test suite; everything must pass.
step "test (debug)" cargo test --workspace --locked --no-fail-fast -- \
  --test-threads=8

# #[ignore]d long tests (portfolio_race routes a real board for minutes in a
# debug build). Off by default; `ci/run-local.sh --long` or FR_LONG_TESTS=1.
# The `alloc_stats` doctest is a ```ignore snippet that only compiles with
# `--features fr-metrics/alloc-stats` — it is documentation, not a test.
if [ "$LONG" = "1" ]; then
  step "test (long, ignored)" cargo test --workspace --locked --no-fail-fast -- \
    --test-threads=8 --include-ignored \
    --skip alloc_stats
fi

step "release build (fr-cli + g21-referee)" \
  cargo build --release --locked -p fr-cli -p g21-referee

step "6-board rust smoke" \
  nice -n 10 python3 bench/tools/ci_smoke.py

log ""
log "======== step timings ========"
if command -v column >/dev/null 2>&1; then
  awk '{printf "%6ss  %s\n", $1, substr($0, index($0,$2))}' "$TIMING"
else
  cat "$TIMING"
fi
log "CI PASS"

#!/usr/bin/env bash
# One-touch acceptance testing script for branch vs mainline regression testing.
#
# Usage:
#   bench/tools/remote/accept.sh <worktree|commit> [--host auto|orca|salmon|whale|<ip>] [--doors full|doors|on|off|default] [--suite 24+oos|24|oos|46] [--allow-deadline-noise <N>]
#
# Features:
#   - --host auto: probes 1-min load average across orca, salmon, whale via ssh and picks the lowest loaded runner.
#                  whale is deprioritized (only selected if both orca and salmon exceed load threshold 4.0).
#   - Executes quality sweep remotely using quality.sh.
#   - Compares resulting gate.json against the latest baseline under bench/results/main-sweeps/
#     using bench/tools/accept_compare.py.
#   - Exits with non-zero code if regressions or added violations occur.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)

if [ $# -lt 1 ]; then
  echo "usage: $0 <worktree|commit> [--host auto|orca|salmon|whale|<ip>] [--doors full|doors|on|off|default] [--suite 24+oos|24|oos|46] [--allow-deadline-noise <N>]" >&2
  exit 1
fi

TARGET=$1
shift

HOST_CHOICE="auto"
DOORS="default"
SUITE="24+oos"
ALLOW_NOISE="0"

while [ $# -gt 0 ]; do
  case "$1" in
    --host)
      HOST_CHOICE="$2"
      shift 2
      ;;
    --doors)
      DOORS="$2"
      shift 2
      ;;
    --suite)
      SUITE="$2"
      shift 2
      ;;
    --allow-deadline-noise)
      ALLOW_NOISE="$2"
      shift 2
      ;;
    *)
      echo "error: unknown argument $1" >&2
      exit 1
      ;;
  esac
done

# 1. Host resolution
SELECTED_HOST=""
SELECTED_IP=""

get_load() {
  local ip="$1"
  local load
  load=$(ssh -o ConnectTimeout=3 -o BatchMode=yes "chiqingzhen@$ip" "cat /proc/loadavg 2>/dev/null" 2>/dev/null | awk '{print $1}')
  echo "${load:-999.0}"
}

if [ "$HOST_CHOICE" = "auto" ]; then
  echo "==> Auto-probing remote host loads (orca: 172.20.0.6, salmon: 172.20.0.2, whale: 172.20.0.4)..."
  ORCA_LOAD=$(get_load "172.20.0.6")
  SALMON_LOAD=$(get_load "172.20.0.2")
  WHALE_LOAD=$(get_load "172.20.0.4")

  echo "    orca (80c):   load $ORCA_LOAD"
  echo "    salmon (64c): load $SALMON_LOAD"
  echo "    whale (8c):   load $WHALE_LOAD"

  # Selection policy: prefer orca and salmon. Only choose whale if both orca and salmon > 4.0
  SELECTED_IP=$(python3 -c "
orca = float('$ORCA_LOAD')
salmon = float('$SALMON_LOAD')
whale = float('$WHALE_LOAD')

if orca <= 4.0 or salmon <= 4.0:
    if orca <= salmon:
        print('172.20.0.6')
    else:
        print('172.20.0.2')
else:
    # Both busy; compare with whale
    lowest = min([('172.20.0.6', orca), ('172.20.0.2', salmon), ('172.20.0.4', whale)], key=lambda x: x[1])
    print(lowest[0])
")
  case "$SELECTED_IP" in
    172.20.0.6) SELECTED_HOST="orca" ;;
    172.20.0.2) SELECTED_HOST="salmon" ;;
    172.20.0.4) SELECTED_HOST="whale" ;;
    *) SELECTED_HOST="$SELECTED_IP" ;;
  esac

  echo "==> Selected host: $SELECTED_HOST ($SELECTED_IP)"
  if [ "$SELECTED_HOST" = "whale" ]; then
    echo "    NOTE: whale has lower single-core frequency (0.74x relative speed). Unrouted counts on deadline-timeout boards are not directly comparable to orca/salmon."
  fi
else
  SELECTED_HOST="$HOST_CHOICE"
fi

# 2. Execute quality sweep(s)
# Note: 24+oos will run 24 first then oos if 24 passed
RUN_SUITES=()
case "$SUITE" in
  24+oos)
    RUN_SUITES=("24" "oos")
    ;;
  24|oos|46)
    RUN_SUITES=("$SUITE")
    ;;
  *)
    echo "error: invalid suite $SUITE (must be 24+oos, 24, oos, or 46)" >&2
    exit 1
    ;;
esac

GATES_COLLECTED=()

for s in "${RUN_SUITES[@]}"; do
  echo "==> Running quality sweep for suite: $s on $SELECTED_HOST (doors: $DOORS)..."
  # Run quality.sh and capture output to extract LOCAL_LOG_DIR
  set +e
  Q_OUT=$("$SCRIPT_DIR/quality.sh" "$TARGET" --host "$SELECTED_HOST" --suite "$s" --doors "$DOORS" 2>&1)
  Q_RC=$?
  set -e

  echo "$Q_OUT"

  # Extract local log directory from quality.sh output: "==> Results saved to: /Users/.../fr-work/logs/remote/<tag>"
  LOG_DIR=$(echo "$Q_OUT" | grep -E "Results saved to: " | tail -n 1 | sed -E 's/.*Results saved to: //')

  if [ -z "$LOG_DIR" ] || [ ! -f "$LOG_DIR/gate.json" ]; then
    echo "error: failed to locate gate.json from quality.sh run in $LOG_DIR" >&2
    exit 2
  fi

  GATES_COLLECTED+=("$LOG_DIR/gate.json")
done

# 3. Compare against latest baseline
OVERALL_EXIT=0
for g in "${GATES_COLLECTED[@]}"; do
  echo ""
  # Detect whether this gate is main or oos based on fixture contents
  IS_OOS=$(python3 -c "
import json, sys
with open('$g') as f:
    d = json.load(f)
fixtures = [r.get('fixture', '') for r in d]
if any('cla45-' in f or 'kicad-' in f or 'easyeda-' in f for f in fixtures):
    print('1')
else:
    print('0')
")

  LATEST_BASELINE=$(python3 -c "
from pathlib import Path
import sys
sys.path.insert(0, '$REPO_ROOT/bench/tools')
import accept_compare

sweeps_dir = Path('$REPO_ROOT') / 'bench' / 'results' / 'main-sweeps'
candidates = []
for entry in sweeps_dir.iterdir():
    if entry.is_dir():
        if '$DOORS' == 'full':
            if '$IS_OOS' == '1':
                gate_path = entry / 'full' / 'oos' / 'gate.json'
            else:
                gate_path = entry / 'full' / 'main' / 'gate.json'
        else:
            if '$IS_OOS' == '1':
                gate_path = entry / 'oos' / 'gate.json'
            else:
                gate_path = entry / 'main' / 'gate.json'
        if gate_path.is_file():
            candidates.append((gate_path.stat().st_mtime, gate_path))

if not candidates and '$IS_OOS' == '1':
    # Fallback for OOS baseline if not yet in main-sweeps
    fallback = Path('$REPO_ROOT') / 'bench' / 'results' / 'gate2-quality' / '20260906-ddc6364' / 'oos' / 'gate.json'
    if fallback.is_file():
        candidates.append((fallback.stat().st_mtime, fallback))

if candidates:
    candidates.sort(key=lambda x: x[0], reverse=True)
    print(candidates[0][1])
else:
    sys.exit(1)
")

  echo "==> Baseline gate for $g: $LATEST_BASELINE"

  set +e
  python3 "$REPO_ROOT/bench/tools/accept_compare.py" \
    "$g" \
    "$LATEST_BASELINE" \
    --allow-deadline-noise "$ALLOW_NOISE" \
    --repo-root "$REPO_ROOT"
  COMP_RC=$?
  set -e
  if [ $COMP_RC -ne 0 ]; then
    OVERALL_EXIT=$COMP_RC
  fi
done

exit $OVERALL_EXIT

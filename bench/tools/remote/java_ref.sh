#!/usr/bin/env bash
# Runs a clean Java reference run (--user_data_path isolated) on orca (172.20.0.6).
#
# Usage:
#   bench/tools/remote/java_ref.sh [--suite 24|46|oos] [--mp 20] [--job-timeout 285] [-w 8]
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)

SUITE="46"
MP="20"
JOB_TIMEOUT="285"
WORKERS="8"
WORKTREE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --suite)
      SUITE="$2"
      shift 2
      ;;
    --mp)
      MP="$2"
      shift 2
      ;;
    --job-timeout)
      JOB_TIMEOUT="$2"
      shift 2
      ;;
    -w|--concurrency)
      WORKERS="$2"
      shift 2
      ;;
    --worktree)
      WORKTREE="$2"
      shift 2
      ;;
    *)
      echo "error: unknown argument $1" >&2
      exit 1
      ;;
  esac
done

REMOTE_HOST=${ORCA_HOST:-"chiqingzhen@172.20.0.6"}
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
RUN_TAG="java-ref-${SUITE}-${TIMESTAMP}"

echo "==> Starting Java reference run: $RUN_TAG on $REMOTE_HOST"

# Copy runner script
ssh "$REMOTE_HOST" mkdir -p "\$HOME/fr/tools" "\$HOME/fr/runs"
scp -q "$SCRIPT_DIR/run_java.py" "$REMOTE_HOST:~/fr/tools/run_java.py"

ssh "$REMOTE_HOST" bash -s <<REMOTE_SCRIPT
set -euo pipefail
RUN_DIR="\$HOME/fr/runs/${RUN_TAG}"
python3 "\$HOME/fr/tools/run_java.py" \
  --suite "${SUITE}" \
  --mp "${MP}" \
  --job-timeout "${JOB_TIMEOUT}" \
  --concurrency "${WORKERS}" \
  --out "\$RUN_DIR"
REMOTE_SCRIPT

LOCAL_LOG_DIR="$HOME/fr-work/logs/remote/${RUN_TAG}"
mkdir -p "$LOCAL_LOG_DIR"
echo "==> Pulling results from $REMOTE_HOST:~/fr/runs/${RUN_TAG} to $LOCAL_LOG_DIR..."
rsync -avz "$REMOTE_HOST:~/fr/runs/${RUN_TAG}/" "$LOCAL_LOG_DIR/"

echo "==> Java reference results saved to: $LOCAL_LOG_DIR"

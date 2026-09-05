#!/usr/bin/env bash
# Dispatches a quality sweep (24 / 46 / OOS) to orca (172.20.0.6).
#
# Usage:
#   bench/tools/remote/quality.sh <worktree_or_commit> [--suite 24|46|oos] [--mp 20] [--job-timeout 285] [--doors on|off] [-w 8]
#
# Examples:
#   bench/tools/remote/quality.sh .
#   bench/tools/remote/quality.sh ~/fr-work/ci --suite 46
#   bench/tools/remote/quality.sh fc3d67f --suite 46
#   bench/tools/remote/quality.sh ~/fr-work/angles --suite 24 --doors on
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)

if [ $# -lt 1 ]; then
  echo "usage: $0 <worktree_or_commit> [--suite 24|46|oos] [--mp 20] [--job-timeout 285] [--doors on|off] [-w 8]" >&2
  exit 1
fi

TARGET=$1
shift

SUITE="46"
MP="20"
JOB_TIMEOUT="285"
DOORS="off"
WORKERS="8"

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
    --doors)
      DOORS="$2"
      shift 2
      ;;
    -w|--concurrency)
      WORKERS="$2"
      shift 2
      ;;
    *)
      echo "error: unknown argument $1" >&2
      exit 1
      ;;
  esac
done

REMOTE_HOST=${ORCA_HOST:-"chiqingzhen@172.20.0.6"}
REMOTE_REPO=${ORCA_REPO:-"~/fr/fast-routing.git"}

# 1. Push worktree or commit to orca
source "$SCRIPT_DIR/push.sh" "$TARGET"
BRANCH="$ORCA_BRANCH"
COMMIT="$ORCA_COMMIT"
SHORT="$ORCA_SHORT"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
SAFE_BRANCH=$(echo "$BRANCH" | tr '/' '_')
RUN_TAG="${SAFE_BRANCH}-${SHORT}-${TIMESTAMP}"

echo "==> Remote run tag: $RUN_TAG on $REMOTE_HOST"

# 2. Stage runner tools on orca
ssh "$REMOTE_HOST" mkdir -p "\$HOME/fr/tools"
scp -q "$SCRIPT_DIR/run_suite.py" "$REPO_ROOT/bench/tools/g21_judge.py" "$REMOTE_HOST:~/fr/tools/"

# 3. On orca: checkout/update worktree, build release binaries, run suite
RC=0
ssh "$REMOTE_HOST" bash -s <<REMOTE_SCRIPT || RC=$?
set -euo pipefail
export PATH="\$HOME/.cargo/bin:\$PATH"

WORKTREE_DIR="\$HOME/fr/worktrees/${SAFE_BRANCH}"
TARGET_DIR="\$HOME/fr/targets/${SAFE_BRANCH}"
RUN_DIR="\$HOME/fr/runs/${RUN_TAG}"

mkdir -p "\$HOME/fr/worktrees" "\$HOME/fr/runs" "\$TARGET_DIR"

if [ ! -d "\$WORKTREE_DIR/.git" ]; then
  echo "==> Cloning branch ${BRANCH} into \$WORKTREE_DIR..."
  git clone "\$HOME/fr/fast-routing.git" "\$WORKTREE_DIR" -b "${BRANCH}"
else
  echo "==> Updating \$WORKTREE_DIR to ${BRANCH} @ ${SHORT}..."
  git -C "\$WORKTREE_DIR" fetch origin "${BRANCH}"
  git -C "\$WORKTREE_DIR" reset --hard "origin/${BRANCH}"
fi

echo "==> Building fr-cli and g21-referee in \$TARGET_DIR..."
t_b0=\$(date +%s)
CARGO_TARGET_DIR="\$TARGET_DIR" cargo build --release --locked \
  --manifest-path "\$WORKTREE_DIR/Cargo.toml" \
  -p fr-cli -p g21-referee
t_b1=\$(date +%s)
echo "    Build finished in \$((t_b1 - t_b0))s"

BIN="\$TARGET_DIR/release/fr-cli"
REFEREE="\$TARGET_DIR/release/g21-referee"

echo "==> Starting suite runner (${SUITE}) with W=${WORKERS}..."
set +e
python3 "\$HOME/fr/tools/run_suite.py" \
  --bin "\$BIN" \
  --referee "\$REFEREE" \
  --suite "${SUITE}" \
  --mp "${MP}" \
  --job-timeout "${JOB_TIMEOUT}" \
  --doors "${DOORS}" \
  --concurrency "${WORKERS}" \
  --out "\$RUN_DIR" \
  --repo-root "\$WORKTREE_DIR"
EXIT_CODE=\$?
set -e

echo "==> Remote runner exited with code \$EXIT_CODE"
exit \$EXIT_CODE
REMOTE_SCRIPT

# 4. Pull results back to Mac (always runs even if gate reported failures)
LOCAL_LOG_DIR="$HOME/fr-work/logs/remote/${RUN_TAG}"
mkdir -p "$LOCAL_LOG_DIR"
echo "==> Pulling results from $REMOTE_HOST:~/fr/runs/${RUN_TAG} to $LOCAL_LOG_DIR..."
rsync -avz "$REMOTE_HOST:~/fr/runs/${RUN_TAG}/" "$LOCAL_LOG_DIR/"

echo "==> Results saved to: $LOCAL_LOG_DIR"
if [ -f "$LOCAL_LOG_DIR/gate.json" ]; then
  echo "==> Gate summary available at $LOCAL_LOG_DIR/gate.json"
fi

exit $RC

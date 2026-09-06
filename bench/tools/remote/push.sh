#!/usr/bin/env bash
# Push the specified worktree or commit to a remote bare repo, preserving branch name.
#
# Usage:
#   bench/tools/remote/push.sh <worktree_or_commit> [--host <host_or_ip>]
#
# Examples:
#   bench/tools/remote/push.sh ~/fr-work/ci
#   bench/tools/remote/push.sh . --host 172.20.0.2
#   bench/tools/remote/push.sh fc3d67f --host salmon
set -euo pipefail

TARGET=${1:-.}
shift || true

HOST_ARG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --host)
      HOST_ARG="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [ -d "$TARGET" ]; then
  ROOT=$(git -C "$TARGET" rev-parse --show-toplevel)
  BRANCH=$(git -C "$TARGET" rev-parse --abbrev-ref HEAD)
  COMMIT=$(git -C "$TARGET" rev-parse HEAD)
  SHORT=$(git -C "$TARGET" rev-parse --short HEAD)
elif [ -d "$HOME/fr-work/$TARGET" ]; then
  TARGET="$HOME/fr-work/$TARGET"
  ROOT=$(git -C "$TARGET" rev-parse --show-toplevel)
  BRANCH=$(git -C "$TARGET" rev-parse --abbrev-ref HEAD)
  COMMIT=$(git -C "$TARGET" rev-parse HEAD)
  SHORT=$(git -C "$TARGET" rev-parse --short HEAD)
else
  # Commit hash or ref in current git repo
  COMMIT=$(git rev-parse "$TARGET")
  SHORT=$(git rev-parse --short "$COMMIT")
  BRANCH="rev-$SHORT"
  ROOT=$(git rev-parse --show-toplevel)
fi

# Resolve host alias or IP
REMOTE_IP="172.20.0.6"
if [ -n "$HOST_ARG" ]; then
  case "$HOST_ARG" in
    orca|172.20.0.6) REMOTE_IP="172.20.0.6" ;;
    salmon|172.20.0.2) REMOTE_IP="172.20.0.2" ;;
    whale|172.20.0.4) REMOTE_IP="172.20.0.4" ;;
    *) REMOTE_IP="$HOST_ARG" ;;
  esac
elif [ -n "${ORCA_HOST:-}" ]; then
  REMOTE_IP="${ORCA_HOST#*@}"
fi

REMOTE_USER=${ORCA_USER:-"chiqingzhen"}
REMOTE_HOST="${REMOTE_USER}@${REMOTE_IP}"
REMOTE_REPO=${ORCA_REPO:-"~/fr/fast-routing.git"}

if [ "$BRANCH" = "HEAD" ]; then
  REF="$COMMIT:refs/heads/detached-$SHORT"
  DEST_BRANCH="detached-$SHORT"
else
  REF="$COMMIT:refs/heads/$BRANCH"
  DEST_BRANCH="$BRANCH"
fi

echo "==> Pushing $DEST_BRANCH ($SHORT) to $REMOTE_HOST:$REMOTE_REPO"
git -C "$ROOT" push -f "$REMOTE_HOST:$REMOTE_REPO" "$REF"
echo "    pushed $DEST_BRANCH @ $SHORT"

export ORCA_BRANCH="$DEST_BRANCH"
export ORCA_COMMIT="$COMMIT"
export ORCA_SHORT="$SHORT"
export REMOTE_IP="$REMOTE_IP"
export REMOTE_HOST="$REMOTE_HOST"

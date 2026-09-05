#!/usr/bin/env bash
# Push the specified worktree's HEAD to orca's bare repo, preserving branch name.
#
# Usage:
#   bench/tools/remote/push.sh <worktree>
#
# Example:
#   bench/tools/remote/push.sh ~/fr-work/ci
#   bench/tools/remote/push.sh .
set -euo pipefail

TARGET=${1:-.}
if [ ! -d "$TARGET" ]; then
  if [ -d "$HOME/fr-work/$TARGET" ]; then
    TARGET="$HOME/fr-work/$TARGET"
  else
    echo "error: worktree directory '$TARGET' not found" >&2
    exit 1
  fi
fi

ROOT=$(git -C "$TARGET" rev-parse --show-toplevel)
BRANCH=$(git -C "$TARGET" rev-parse --abbrev-ref HEAD)
COMMIT=$(git -C "$TARGET" rev-parse HEAD)
SHORT=$(git -C "$TARGET" rev-parse --short HEAD)

REMOTE_HOST=${ORCA_HOST:-"chiqingzhen@172.20.0.6"}
REMOTE_REPO=${ORCA_REPO:-"~/fr/fast-routing.git"}

if [ "$BRANCH" = "HEAD" ]; then
  REF="HEAD:refs/heads/detached-$SHORT"
  DEST_BRANCH="detached-$SHORT"
else
  REF="HEAD:refs/heads/$BRANCH"
  DEST_BRANCH="$BRANCH"
fi

echo "==> Pushing $BRANCH ($SHORT) to $REMOTE_HOST:$REMOTE_REPO"
git -C "$ROOT" push -f "$REMOTE_HOST:$REMOTE_REPO" "$REF"
echo "    pushed $DEST_BRANCH @ $SHORT"

# Export variables for caller scripts if sourced
export ORCA_BRANCH="$DEST_BRANCH"
export ORCA_COMMIT="$COMMIT"
export ORCA_SHORT="$SHORT"

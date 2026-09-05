#!/usr/bin/env bash
# Push the specified worktree or commit to orca's bare repo, preserving branch name.
#
# Usage:
#   bench/tools/remote/push.sh <worktree_or_commit>
#
# Example:
#   bench/tools/remote/push.sh ~/fr-work/ci
#   bench/tools/remote/push.sh .
#   bench/tools/remote/push.sh fc3d67f
set -euo pipefail

TARGET=${1:-.}
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

REMOTE_HOST=${ORCA_HOST:-"chiqingzhen@172.20.0.6"}
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

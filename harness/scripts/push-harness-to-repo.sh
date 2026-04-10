#!/usr/bin/env bash
set -euo pipefail

# Split the harness/ subtree from an abox branch and push it to a standalone repo.
#
# Usage:
#   harness/scripts/push-harness-to-repo.sh <target_remote_url> [source_branch] [target_branch]
#
# Example:
#   harness/scripts/push-harness-to-repo.sh https://github.com/X-McKay/bakudo.git work main

TARGET_REMOTE_URL="${1:-}"
SOURCE_BRANCH="${2:-work}"
TARGET_BRANCH="${3:-main}"
SPLIT_BRANCH="bakudo-sync-${SOURCE_BRANCH}-$(date +%Y%m%d%H%M%S)"

if [[ -z "${TARGET_REMOTE_URL}" ]]; then
  echo "error: missing target remote URL" >&2
  echo "usage: $0 <target_remote_url> [source_branch] [target_branch]" >&2
  exit 1
fi

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "${ROOT_DIR}"

git show-ref --verify --quiet "refs/heads/${SOURCE_BRANCH}" || {
  echo "error: source branch '${SOURCE_BRANCH}' does not exist" >&2
  exit 1
}

cleanup() {
  git branch -D "${SPLIT_BRANCH}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[1/3] Creating split branch '${SPLIT_BRANCH}' from ${SOURCE_BRANCH}:harness ..."
git subtree split --prefix=harness "${SOURCE_BRANCH}" -b "${SPLIT_BRANCH}" >/dev/null

echo "[2/3] Pushing split branch to ${TARGET_REMOTE_URL} (${TARGET_BRANCH}) ..."
git push "${TARGET_REMOTE_URL}" "${SPLIT_BRANCH}:${TARGET_BRANCH}"

echo "[3/3] Done. Harness subtree from '${SOURCE_BRANCH}' is now pushed to '${TARGET_REMOTE_URL}' branch '${TARGET_BRANCH}'."

#!/usr/bin/env bash
# Clone the four evaluation corpora into the scratch directory.
#
# `--no-hardlinks` keeps the clone's object store independent of the sibling
# checkout, so nothing this evaluation does can touch the live repositories.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

mkdir -p "$REPOS" "$CACHE" "$OUT" "$LOGS"
for repo in orbit orbit-graph observatory orrery; do
  if [ -d "$REPOS/$repo/.git" ]; then
    echo "already cloned: $REPOS/$repo"
    continue
  fi
  git clone --no-hardlinks --quiet "file://$CODEBASES/$repo" "$REPOS/$repo"
  echo "cloned $repo -> $REPOS/$repo ($(git -C "$REPOS/$repo" rev-list --count HEAD) commits)"
done

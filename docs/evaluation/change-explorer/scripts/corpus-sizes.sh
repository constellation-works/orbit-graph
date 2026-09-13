#!/usr/bin/env bash
# Corpus size table for performance.md: commits, tracked files, and per-language
# file/LOC counts at the head revision of each study.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

count_lines() { # count_lines <repo-dir> <rev> <extension>
  local dir="$1" rev="$2" ext="$3" total=0 f
  while IFS= read -r f; do
    total=$((total + $(git -C "$dir" cat-file blob "$rev:$f" | wc -l)))
  done < <(git -C "$dir" ls-tree -r --name-only "$rev" | grep -E "\\.${ext}\$" || true)
  printf '%s' "$total"
}

printf 'study\trepo\tcommits\thead_sha\tfiles\trust_files\trust_loc\tpython_files\tpython_loc\tother_files\n'
for line in "${STUDIES[@]}"; do
  id=$(study_field "$line" 1); repo=$(study_field "$line" 2); head=$(study_field "$line" 4)
  dir="$REPOS/$repo"
  files=$(git -C "$dir" ls-tree -r --name-only "$head" | wc -l)
  rustf=$(git -C "$dir" ls-tree -r --name-only "$head" | grep -cE '\.rs$' || true)
  pyf=$(git -C "$dir" ls-tree -r --name-only "$head" | grep -cE '\.py$' || true)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$id" "$repo" \
    "$(git -C "$dir" rev-list --count HEAD)" "${head:0:12}" \
    "$files" "$rustf" "$(count_lines "$dir" "$head" rs)" \
    "$pyf" "$(count_lines "$dir" "$head" py)" "$((files - rustf - pyf))"
done

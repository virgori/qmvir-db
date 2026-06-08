#!/usr/bin/env bash
set -euo pipefail

root="${1:-.}"
root="${root%/}"

found=0

find "$root" \
  \( -path "$root/.git" -o -path "$root/target" -o -path "$root/qmvir-studio/src-tauri/target" \) -prune \
  -o -name '* [0-9].*' -type f -print |
  sort |
  while IFS= read -r file; do
    canonical="$(printf '%s\n' "$file" | sed -E 's/ [0-9]+(\.[^./]+)$/\1/')"
    found=1

    if [ -f "$canonical" ]; then
      if cmp -s "$file" "$canonical"; then
        status="IDENTICAL"
      else
        status="DIFFERENT"
      fi
    else
      status="NO_CANON"
    fi

    printf '%s\t%s\t%s\n' "$status" "$file" "$canonical"
  done

if find "$root" \
  \( -path "$root/.git" -o -path "$root/target" -o -path "$root/qmvir-studio/src-tauri/target" \) -prune \
  -o -name '* [0-9].*' -type f -print -quit | grep -q .; then
  echo "error: duplicate space-number files found" >&2
  exit 1
fi

exit "$found"

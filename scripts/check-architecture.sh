#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

src=crates/goku-core/src
fail=0

os_imports=$(grep -nE '^\s*use (screencapturekit|axuielement|objc2[a-z_]*|core_graphics|core_foundation|windows|uiautomation)(::|;)' "$src"/*.rs \
  | grep -vE "^$src/(macos|windows)\.rs:" || true)
if [ -n "$os_imports" ]; then
  echo "OS crates may only be used in macos.rs and windows.rs:"
  echo "$os_imports"
  fail=1
fi

for pure in types error filter redact triggers imagebuf commit; do
  bad=$(grep -nE 'crate::(store|engine|platform|macos|windows)\b' "$src/$pure.rs" || true)
  if [ -n "$bad" ]; then
    echo "$pure.rs must stay pure. It may not import store, engine, platform, macos, or windows:"
    echo "$bad"
    fail=1
  fi
done

while IFS= read -r file; do
  lines=$(wc -l < "$file" | tr -d ' ')
  if [ "$lines" -gt 1500 ]; then
    echo "$file has $lines lines. The limit is 1500. Split it."
    fail=1
  fi
done < <(find crates bindings examples -name '*.rs' -not -path '*/target/*' -not -path '*/node_modules/*')

if [ -n "${GOKU_BASE_REF:-}" ] && [ "${GOKU_ALLOW_WIDE_CHANGE:-0}" != "1" ]; then
  changed=$(git diff --name-only "origin/$GOKU_BASE_REF...HEAD" \
    | grep -vE '(^|/)tests/|\.md$|\.tsv$|Cargo\.lock$|package-lock\.json$|^docs/|^verify/' || true)
  count=$(printf '%s\n' "$changed" | grep -c . || true)
  if [ "$count" -gt 12 ]; then
    echo "This change touches $count source files. The limit is 12. Split it, or ask the operator for the wide-change label."
    printf '%s\n' "$changed"
    fail=1
  fi
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "architecture checks passed"

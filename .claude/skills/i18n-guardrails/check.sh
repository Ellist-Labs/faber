#!/usr/bin/env bash
# Scan faber-app/src for suspect hardcoded user-facing strings.
# False positives are expected — use judgment. Run from repo root.
set -euo pipefail

SRC="crates/faber-app/src"
PASS=true

echo "=== i18n guardrail scan: $SRC ==="
echo ""

# Pattern: string literals passed to display-bearing APIs
# Looks for .child("..."), .child(format!(...)) with plain strings,
# Label::new("..."), placeholder("..."), MenuItem::action("...", ...)
SUSPECTS=$(rg --with-filename --line-number \
  '\.child\("(?!replace-|search-|tab-|sidebar-|titlebar|file-tree|settings-scroll|explorer-|outline|close-btn|toggle-)[^"]{3,}"' \
  "$SRC" \
  --glob '*.rs' \
  --ignore-case || true)

if [ -n "$SUSPECTS" ]; then
  echo "Possible untranslated .child() calls:"
  echo "$SUSPECTS" | head -40
  echo ""
  PASS=false
fi

# Check for format! strings that look like user messages (not log lines)
FORMAT_SUSPECTS=$(rg --with-filename --line-number \
  'format!\("[A-Z][a-z]' \
  "$SRC" \
  --glob '*.rs' | grep -v 'eprintln\|println\|log\|debug\|error\|warn\|info\|FABER_READY' || true)

if [ -n "$FORMAT_SUSPECTS" ]; then
  echo "Possible untranslated format! strings:"
  echo "$FORMAT_SUSPECTS" | head -20
  echo ""
  PASS=false
fi

if $PASS; then
  echo "No obvious hardcoded strings found."
else
  echo "Review the matches above. False positives are expected."
  echo "Fix genuine hits: add a key to locales/en.toml and use t!(\"key\")."
fi

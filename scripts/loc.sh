#!/usr/bin/env bash
set -euo pipefail

MAX_LOC=1500
FAILED=0

# Find all rust files, excluding target/ and .git/
while IFS= read -r file; do
    # Count lines of code excluding comments and blank lines
    LOC=$(grep -vE '^\s*//|^\s*$' "$file" | wc -l || true)
    if [ "$LOC" -gt "$MAX_LOC" ]; then
        echo "ERROR: $file exceeds $MAX_LOC lines of code: $LOC"
        FAILED=1
    fi
done < <(find . -name "*.rs" -not -path "*/target/*" -not -path "*/.git/*")

if [ "$FAILED" -ne 0 ]; then
    echo "LOC check failed! Keep all source files within 1,500 lines."
    exit 1
fi

echo "LOC check passed: all Rust source files are within $MAX_LOC lines."

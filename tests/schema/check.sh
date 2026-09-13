#!/usr/bin/env bash
# Checks schema/recipe.schema.json against the sample recipes.
# Every file in valid/ must pass. Every file in invalid/ must fail.
# Needs check-jsonschema on PATH (pip install check-jsonschema).
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
schema="$root/schema/recipe.schema.json"
failed=0

check-jsonschema --check-metaschema "$schema" || failed=1

for f in "$root"/tests/schema/valid/*.json; do
  if ! check-jsonschema -q --schemafile "$schema" "$f"; then
    echo "FAIL: $f should pass"
    failed=1
  fi
done

for f in "$root"/tests/schema/invalid/*.json; do
  if check-jsonschema -q --schemafile "$schema" "$f" >/dev/null 2>&1; then
    echo "FAIL: $f should be rejected"
    failed=1
  fi
done

# Rule samples break only rules the schema cannot express, so the schema must accept them.
for f in "$root"/tests/recipe/invalid/*.json; do
  if ! check-jsonschema -q --schemafile "$schema" "$f"; then
    echo "FAIL: $f should pass the schema (it tests a rule the schema cannot express)"
    failed=1
  fi
done

if [ "$failed" -eq 0 ]; then echo "schema samples ok"; fi
exit "$failed"

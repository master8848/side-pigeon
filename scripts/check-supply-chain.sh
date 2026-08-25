#!/usr/bin/env bash
# Supply-chain gate: every crate in Cargo.lock must have been created on
# crates.io at least 14 days ago (per CONTRIBUTING.md dependency policy).
#
# Usage: ./scripts/check-supply-chain.sh
set -euo pipefail
cd "$(dirname "$0")/.."

MIN_AGE_DAYS=14
UA="provider-connect supply-chain-check (dev)"

# Local path crates (provider-core, provider-ffi, provider-transport, etc.) are
# not published to crates.io and are thus not age-gated directly; their
# transitive registry dependencies (including `provider-ffi` optional deps like
# `rusqlite` behind `persist`) ARE gated via the resolved graph below. Use
# --all-features so optional deps (persist/ws/http) are included in metadata.
if cargo metadata --format-version 1 --all-features >/dev/null 2>&1; then
  META_CMD=(cargo metadata --format-version 1 --all-features)
else
  META_CMD=(cargo metadata --format-version 1)
fi

# Unique registry crate names from the resolved dependency graph.
CRATES=$("${META_CMD[@]}" 2>/dev/null | python3 -c '
import json, sys
meta = json.load(sys.stdin)
names = sorted({p["name"] for p in meta["packages"] if (p.get("source") or "").startswith("registry")})
print("\n".join(names))
')

if [[ -z "$CRATES" ]]; then
  echo "no registry crates found" >&2
  exit 1
fi

now_epoch=$(date +%s)
cutoff=$(( now_epoch - MIN_AGE_DAYS * 86400 ))
fail=0
declare -a rows

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  created=$(curl -sS -H "User-Agent: $UA" "https://crates.io/api/v1/crates/$name" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    print(data.get("crate", {}).get("created_at", ""))
except Exception:
    print("")
')
  if [[ -z "$created" ]]; then
    echo "FAIL  $name: could not fetch created_at" >&2
    fail=1
    continue
  fi
  created_epoch=$(date -j -f "%Y-%m-%dT%H:%M:%S" "${created:0:19}" +%s 2>/dev/null     || date -d "$created" +%s)
  age_days=$(( (now_epoch - created_epoch) / 86400 ))
  rows+=("$name $created ${age_days}d")
  if (( age_days < MIN_AGE_DAYS )); then
    echo "FAIL  $name: created $created (${age_days}d < ${MIN_AGE_DAYS}d)" >&2
    fail=1
  fi
done <<< "$CRATES"

printf "%-40s %-28s %s\n" "crate" "created_at" "age"
printf "%s\n" "----------------------------------------------------------------------"
for row in "${rows[@]}"; do
  printf "%-40s %-28s %s\n" $row
done

if (( fail )); then
  echo "SUPPLY-CHAIN CHECK FAILED (crates newer than ${MIN_AGE_DAYS} days)" >&2
  exit 1
fi
echo "OK: ${#rows[@]} crates, all created >= ${MIN_AGE_DAYS} days ago"

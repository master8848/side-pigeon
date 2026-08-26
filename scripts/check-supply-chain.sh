#!/usr/bin/env bash
# Supply-chain gate: every registry crate version in Cargo.lock must have been
# published on crates.io at least 14 days ago (per CONTRIBUTING.md dependency
# policy). Checks version created_at via /crates/$name/$version, not crate
# first-publish time, so a newly-published version of an old crate is still
# gated. Covers both the root workspace and the standalone cli/ workspace.
#
# Usage: ./scripts/check-supply-chain.sh
set -euo pipefail
cd "$(dirname "$0")/.."

MIN_AGE_DAYS=14
UA="provider-connect supply-chain-check (dev)"

# Build metadata for root workspace and standalone cli/ workspace (cli/Cargo.toml
# is its own [workspace] root, not a root member). Unique (name, version) pairs
# from both workspaces are unioned so CLI-only deps are not silently exempt.
collect_packages() {
  local manifest="$1"
  local meta_args=(cargo metadata --format-version 1 --all-features --manifest-path "$manifest")
  if ! "${meta_args[@]}" >/dev/null 2>&1; then
    meta_args=(cargo metadata --format-version 1 --manifest-path "$manifest")
  fi
  "${meta_args[@]}" 2>/dev/null
}

TMP_ROOT=$(mktemp)
TMP_CLI=$(mktemp)
trap 'rm -f "$TMP_ROOT" "$TMP_CLI"' EXIT

collect_packages "Cargo.toml" > "$TMP_ROOT"
if [[ -f "cli/Cargo.toml" ]]; then
  collect_packages "cli/Cargo.toml" > "$TMP_CLI"
else
  echo '{"packages":[]}' > "$TMP_CLI"
fi

CRATES=$(python3 <<PY
import json, pathlib
roots = [pathlib.Path("$TMP_ROOT"), pathlib.Path("$TMP_CLI")]
seen = set()
for p in roots:
    try:
        meta = json.loads(p.read_text())
    except Exception:
        continue
    for pkg in meta.get("packages", []):
        src = pkg.get("source") or ""
        if src.startswith("registry"):
            seen.add((pkg["name"], pkg["version"]))
for name, ver in sorted(seen):
    print(f"{name} {ver}")
PY
)

if [[ -z "$CRATES" ]]; then
  echo "no registry crates found" >&2
  exit 1
fi

now_epoch=$(date +%s)
cutoff=$(( now_epoch - MIN_AGE_DAYS * 86400 ))
fail=0
declare -a rows

while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  name=$(awk '{print $1}' <<< "$line")
  version=$(awk '{print $2}' <<< "$line")
  [[ -z "$name" || -z "$version" ]] && continue
  # Per-version endpoint: gates a freshly-published version of an old crate.
  created=$(curl -sS -H "User-Agent: $UA" "https://crates.io/api/v1/crates/$name/$version" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    # /crates/<name>/<version> returns {"version": {"created_at": "..."}}
    # Fall back to crate.created_at for older API shapes (should not happen).
    v = data.get("version") or {}
    c = v.get("created_at") or data.get("crate", {}).get("created_at", "")
    print(c)
except Exception:
    print("")
')
  if [[ -z "$created" ]]; then
    echo "FAIL  $name $version: could not fetch created_at for version" >&2
    fail=1
    continue
  fi
  created_epoch=$(date -j -f "%Y-%m-%dT%H:%M:%S" "${created:0:19}" +%s 2>/dev/null     || date -d "$created" +%s)
  age_days=$(( (now_epoch - created_epoch) / 86400 ))
  rows+=("$name $version $created ${age_days}d")
  if (( age_days < MIN_AGE_DAYS )); then
    echo "FAIL  $name $version: created $created (${age_days}d < ${MIN_AGE_DAYS}d)" >&2
    fail=1
  fi
done <<< "$CRATES"

printf "%-40s %-12s %-28s %s\n" "crate" "version" "created_at" "age"
printf "%s\n" "--------------------------------------------------------------------------------"
for row in "${rows[@]}"; do
  # shellcheck disable=SC2086
  printf "%-40s %-12s %-28s %s\n" $row
done

if (( fail )); then
  echo "SUPPLY-CHAIN CHECK FAILED (crates newer than ${MIN_AGE_DAYS} days)" >&2
  exit 1
fi
echo "OK: ${#rows[@]} crates, all versions created >= ${MIN_AGE_DAYS} days ago"

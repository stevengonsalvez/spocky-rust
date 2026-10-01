#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359

if [ -n "${PASEO_HUB_BASELINE_ROOT:-}" ]; then
  baseline_root=$PASEO_HUB_BASELINE_ROOT
else
  canonical="$repository_root/.baselines/hub"
  worktree_sibling="$repository_root/../../paseo-rust/.baselines/hub"
  found=0
  for candidate in "$canonical" "$worktree_sibling"; do
    if [ -d "$candidate/.git" ] || git -C "$candidate" rev-parse --git-dir >/dev/null 2>&1; then
      baseline_root=$candidate
      found=$((found + 1))
    fi
  done
  if [ "$found" -ne 1 ]; then
    printf 'expected exactly one Hub baseline, found %s\n' "$found" >&2
    exit 1
  fi
fi

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
[ "$actual" = "$expected_baseline" ] || { printf 'baseline commit mismatch: %s\n' "$actual" >&2; exit 1; }
[ -z "$(gtimeout 30 git -C "$baseline_root" status --porcelain)" ] || { printf 'baseline is dirty\n' >&2; exit 1; }

fixture=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-schema-downgrade.XXXXXX")
cleanup() {
  case "$fixture" in
    "${TMPDIR:-/tmp}"/spocky-hub-schema-downgrade.*) gtimeout 30 rm -rf "$fixture" ;;
    *) printf 'refusing unsafe cleanup: %s\n' "$fixture" >&2; exit 1 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

gtimeout --kill-after=30 300 git -C "$baseline_root" archive -o "$fixture/baseline.tar" "$expected_baseline"
mkdir "$fixture/source" "$fixture/source-future"
gtimeout 60 tar -xf "$fixture/baseline.tar" -C "$fixture/source"
gtimeout 60 tar -xf "$fixture/baseline.tar" -C "$fixture/source-future"
(cd "$fixture/source" && gtimeout --kill-after=30 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
ln -s "$fixture/source/node_modules" "$fixture/source-future/node_modules"

make_future() {
  target=$1
  count=$2
  cp -R "$fixture/source/drizzle" "$target"
  node - "$target" "$count" <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const [root, countText] = process.argv.slice(2);
const journalPath = path.join(root, "meta", "_journal.json");
const journal = JSON.parse(fs.readFileSync(journalPath, "utf8"));
const last = journal.entries.at(-1);
for (let offset = 1; offset <= Number(countText); offset += 1) {
  const index = last.idx + offset;
  const tag = `${String(index).padStart(4, "0")}_schema_downgrade_${offset}`;
  journal.entries.push({ idx: index, version: last.version, when: last.when + offset, tag, breakpoints: true });
  const sql = offset === 1
    ? "create table schema_downgrade_partial (value text not null);\n--> statement-breakpoint\ninsert into schema_downgrade_partial (value) values ('pending')"
    : "this is deliberately invalid sql";
  fs.writeFileSync(path.join(root, `${tag}.sql`), `${sql}\n`);
}
fs.writeFileSync(journalPath, `${JSON.stringify(journal, null, 2)}\n`);
NODE
}

make_future "$fixture/future-one" 1
rm -rf "$fixture/source-future/drizzle"
cp -R "$fixture/future-one" "$fixture/source-future/drizzle"
make_future "$fixture/future-failing" 2

gtimeout --kill-after=30 300 cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-schema-downgrade-evidence >/dev/null
candidate="$repository_root/target/debug/hub-schema-downgrade-evidence"
node_bin=$(command -v node)
package="$fixture/source/node_modules/@electric-sql/pglite"
adapter="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs"
run_candidate() {
  migrations=$1
  mode=$2
  database=$3
  SPOCKY_NODE="$node_bin" SPOCKY_PGLITE_ADAPTER="$adapter" SPOCKY_PGLITE_PACKAGE="$package" \
    SPOCKY_HUB_MIGRATIONS="$migrations" gtimeout --kill-after=30 300 "$candidate" "$mode" "$database"
}
run_baseline() {
  source=$1
  mode=$2
  database=$3
  PASEO_HUB_SOURCE_ROOT="$source" gtimeout --kill-after=30 300 \
    "$fixture/source/node_modules/.bin/tsx" "$repository_root/scripts/phase2/hub-schema-downgrade-baseline.mjs" "$mode" "$database"
}

mkdir "$fixture/db-candidate-newer" "$fixture/db-baseline-newer" "$fixture/db-failure"
run_candidate "$fixture/future-one" produce "$fixture/db-candidate-newer" >"$fixture/candidate-newer-produce.json"
run_baseline "$fixture/source" observe "$fixture/db-candidate-newer" >"$fixture/candidate-newer-legacy.json"

run_baseline "$fixture/source-future" produce "$fixture/db-baseline-newer" >"$fixture/baseline-newer-produce.json"
run_candidate "$fixture/source/drizzle" observe "$fixture/db-baseline-newer" >"$fixture/baseline-newer-candidate.json"

run_candidate "$fixture/source/drizzle" produce "$fixture/db-failure" >"$fixture/failure-seed.json"
run_candidate "$fixture/future-failing" failure "$fixture/db-failure" >"$fixture/failure.json"

baseline_clean=false
if [ "$expected_baseline" = "$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)" ] && \
   [ -z "$(gtimeout 30 git -C "$baseline_root" status --porcelain)" ]; then
  baseline_clean=true
fi

jq -n \
  --arg commit "$expected_baseline" --argjson clean "$baseline_clean" \
  --slurpfile cn "$fixture/candidate-newer-produce.json" \
  --slurpfile cl "$fixture/candidate-newer-legacy.json" \
  --slurpfile bn "$fixture/baseline-newer-produce.json" \
  --slurpfile bc "$fixture/baseline-newer-candidate.json" \
  --slurpfile pf "$fixture/failure.json" '
  {
    baseline: {commit: $commit, clean: $clean},
    candidateNewer: {
      candidateApplied: $cn[0].migration.applied,
      legacyApplied: ($cl[0].after - $cl[0].before),
      journal: $cl[0].after,
      dataPreserved: ([$cl[0].rows[].producer] | contains(["candidate-newer", "baseline-older"]))
    },
    legacyNewer: {
      legacyApplied: ($bn[0].after - $bn[0].before),
      candidateApplied: $bc[0].migration.applied,
      journal: $bc[0].after,
      dataPreserved: ([$bc[0].rows[][0]] | contains(["baseline-newer", "candidate-older"]))
    },
    partialFailure: {
      errorObserved: ($pf[0].error | length > 0),
      journalRolledBack: ($pf[0].before == 49 and $pf[0].after == 49),
      schemaRolledBack: ($pf[0].partialTable == "")
    },
    behavior: "future maximum created_at is accepted and known older migrations are skipped",
    scope: "bounded-additive-only-no-full-parity-claim"
  }' >"$fixture/report.json"

cp "$fixture/report.json" "$repository_root/evidence/phase2/hub-schema-downgrade-report.json"
cat "$fixture/report.json"

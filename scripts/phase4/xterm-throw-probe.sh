#!/bin/sh
# Throw-site probe for spocky-xterm: which xterm exception sites (`Throw`) do
# the corpus, the default byte fuzz and the biased exception fuzz reach?
#
# The probe is a patch to crates/spocky-xterm/src (embedded below) that prints
# one `THROW-SITE <file>:<line>:<col>` line to stderr wherever the port throws,
# using #[track_caller] so the line is the caller's. The script exports the
# committed HEAD into a scratch directory with `git archive`, applies the patch
# there, runs the three differential tests with --nocapture, and counts the
# sites per run. The working tree is never touched, so even a SIGKILL leaves
# only a temporary directory behind. Uncommitted changes are not probed.
#
# Usage (Node 22.20.0), through the build gate:
#   CARGO_TARGET_DIR=/private/tmp/spocky-targets/p4_xterm_core CARGO_BUILD_JOBS=3 \
#     SPOCKY_PINNED_NODE=$HOME/.nvm/versions/node/v22.20.0/bin/node \
#     /private/tmp/spocky-targets/build-gate.sh scripts/phase4/xterm-throw-probe.sh
#
# SPOCKY_XTERM_FUZZ_SEEDS (default 5000) and SPOCKY_XTERM_BIASED_SEEDS
# (default 1000) set the seed counts of the two fuzz runs.
set -u

: "${SPOCKY_PINNED_NODE:?set SPOCKY_PINNED_NODE}"
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root" || exit 1
src=crates/spocky-xterm/src

work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM
tree="$work/tree"
patch="$work/probe.patch"
mkdir "$tree"
git archive HEAD | tar -x -C "$tree"

cat > "$patch" <<'EOF_PATCH'
diff --git a/crates/spocky-xterm/src/buffer.rs b/crates/spocky-xterm/src/buffer.rs
index 1d1928b1..b88b3e79 100644
--- a/crates/spocky-xterm/src/buffer.rs
+++ b/crates/spocky-xterm/src/buffer.rs
@@ -23,7 +23,11 @@ pub(crate) const SCROLLBACK: i64 = 1000;
 const TAB_STOP_WIDTH: i64 = 8;
 
 /// Turns a JavaScript `undefined` line into the `TypeError` its use throws.
+#[track_caller]
 pub(crate) fn req(line: Option<LineRef>) -> Result<LineRef, Throw> {
+    if line.is_none() {
+        eprintln!("THROW-SITE {}", std::panic::Location::caller());
+    }
     line.ok_or(Throw)
 }
 
@@ -106,6 +110,7 @@ impl Buffer {
         self.setup_tab_stops(None);
     }
 
+    #[track_caller]
     pub(crate) fn line(&self, index: i64) -> Result<LineRef, Throw> {
         req(self.lines.get(index))
     }
@@ -496,6 +501,7 @@ pub(crate) fn copy_cells(
     }
 }
 
+#[track_caller]
 fn wrapped_line(lines: &[Option<LineRef>], index: i64) -> Result<LineRef, Throw> {
     req(usize::try_from(index)
         .ok()
diff --git a/crates/spocky-xterm/src/circular_list.rs b/crates/spocky-xterm/src/circular_list.rs
index 354f2b4e..5c58139a 100644
--- a/crates/spocky-xterm/src/circular_list.rs
+++ b/crates/spocky-xterm/src/circular_list.rs
@@ -140,6 +140,7 @@ impl CircularList {
         self.length -= count;
     }
 
+    #[track_caller]
     pub(crate) fn shift_elements(
         &mut self,
         start: i64,
@@ -150,6 +151,11 @@ impl CircularList {
             return Ok(());
         }
         if start < 0 || start >= self.length || start + offset < 0 {
+            eprintln!(
+                "THROW-SITE shift_elements({start},{count},{offset}) len={} caller={}",
+                self.length,
+                std::panic::Location::caller()
+            );
             return Err(Throw);
         }
         if offset > 0 {
diff --git a/crates/spocky-xterm/src/input_handler.rs b/crates/spocky-xterm/src/input_handler.rs
index e53172a3..bc55a3bb 100644
--- a/crates/spocky-xterm/src/input_handler.rs
+++ b/crates/spocky-xterm/src/input_handler.rs
@@ -653,7 +653,10 @@ impl Terminal {
         let length = usize::try_from(param_or_one(params, 0)).unwrap_or(0);
         let x = self.active().x - i64::from(extract_width(join_state));
         let line = req(self.row_line(self.active().y))?;
-        let text = line.borrow().get_string(x).ok_or(Throw)?;
+        let text = line.borrow().get_string(x).ok_or_else(|| {
+            eprintln!("THROW-SITE repeat_preceding_character get_string undefined");
+            Throw
+        })?;
         let codes: Vec<u32> = text.chars().map(u32::from).collect();
         let total = codes.len() * length;
         self.print(&|index| codes[index % codes.len()], 0, total)
EOF_PATCH

# Candidate sites in the clean source: every `req(` call, `.line(` call,
# `wrapped_line(` call, `shift_elements(` call, range check and `ok_or(Throw)`,
# outside the helper bodies that forward the caller location.
echo "== candidate sites (file:line function)"
for file in buffer circular_list input_handler; do
  awk -v f="$file.rs" '
    /^ *(pub\(crate\) )?fn [a-z_0-9]+/ {
      name = $0; sub(/^ *(pub\(crate\) )?fn /, "", name); sub(/[^a-z_0-9].*/, "", name)
      current = name; next
    }
    /^ *\/\// { next }
    /req\(|Err\(Throw\)|\.line\(|wrapped_line\(|shift_elements\(|ok_or\(Throw\)/ {
      if (current == "req" || current == "line" || current == "wrapped_line") next
      print f ":" NR " " current
    }' "$tree/$src/$file.rs"
done > "$work/candidates.txt"
cat "$work/candidates.txt"
echo "candidates: $(wc -l < "$work/candidates.txt" | tr -d ' ')"

(cd "$tree" && git apply --check "$patch" && git apply "$patch") || exit 1
cd "$tree" || exit 1

run() {
  label=$1; shift
  echo "== $label"
  "$@" > "$work/$label.out" 2> "$work/$label.err"
  status=$?
  grep -E "full matches" "$work/$label.err"
  echo "exit: $status"
  grep '^THROW-SITE' "$work/$label.err" | sed "s#$tree/##" | sort | uniq -c | sort -rn
  echo "throw lines: $(grep -c '^THROW-SITE' "$work/$label.err")"
}

run corpus cargo test --locked --offline -p spocky-xterm --test xterm_corpus -- --nocapture
run fuzz env "SPOCKY_XTERM_FUZZ_SEEDS=${SPOCKY_XTERM_FUZZ_SEEDS:-5000}" \
  cargo test --locked --offline -p spocky-xterm --test xterm_fuzz seeded -- --nocapture
run biased env "SPOCKY_XTERM_BIASED_SEEDS=${SPOCKY_XTERM_BIASED_SEEDS:-1000}" \
  cargo test --locked --offline -p spocky-xterm --test xterm_fuzz biased_exception_fuzz -- --nocapture

echo "== reached sites (union)"
cat "$work"/corpus.err "$work"/fuzz.err "$work"/biased.err \
  | grep '^THROW-SITE' | sed "s#$tree/##" | sed -E 's/:[0-9]+$//' | sort -u

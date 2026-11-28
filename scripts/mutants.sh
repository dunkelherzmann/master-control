#!/bin/sh
# Mutation testing with cargo-mutants on the Rust core: changes the code in small ways (a `+` to `-`,
# a function body to `Default::default()`) and checks that some test fails. A "missed" mutant is code
# the tests don't really check. Reports only; it doesn't block a merge.
#
#   scripts/mutants.sh            mutants in what this branch changes against BASE
#   scripts/mutants.sh --all      every mutant in the core (slow)
#   scripts/mutants.sh -f FILE    mutants in one file (path relative to the repo root)
#
# MUTANTS_BASE and MUTANTS_JOBS override the base branch and the number of parallel jobs.
# Results: .build/mutants/mutants.out/ (missed.txt, caught.txt, timeout.txt). Needs
# `cargo install --locked cargo-mutants`.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2
PACKAGE="mc-core"
BASE="${MUTANTS_BASE:-main}"
OUT="$ROOT/.build/mutants"
mkdir -p "$OUT"
PATH="$HOME/.cargo/bin:$PATH"
if ! cargo mutants --version >/dev/null 2>&1; then
    echo "mutants: cargo-mutants isn't installed; run: cargo install --locked cargo-mutants"
    exit 2
fi
# Each job copies the tree and runs the tests; half the performance cores leaves the Mac usable.
JOBS=$(( $(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null || nproc 2>/dev/null || echo 4) / 2 ))
[ "$JOBS" -ge 1 ] || JOBS=1
COMMON="--package $PACKAGE --output $OUT --no-shuffle --timeout-multiplier 3 --jobs ${MUTANTS_JOBS:-$JOBS}"
case "${1:-}" in
    --all) shift ;;
    -f) ;;
    *)
        git diff "$BASE"...HEAD -- '*.rs' >"$OUT/branch.diff"
        if [ ! -s "$OUT/branch.diff" ]; then
            echo "mutants: no Rust changes against $BASE"
            exit 0
        fi
        set -- --in-diff "$OUT/branch.diff" "$@" ;;
esac
# shellcheck disable=SC2086
set -- $COMMON "$@"
R="$OUT/mutants.out"
rm -rf "$R" "$R.old"
cargo mutants "$@" >"$OUT/run.log" 2>&1
code=$?
if [ ! -d "$R" ]; then
    grep -q 'No mutants' "$OUT/run.log" && { echo "mutants: nothing to mutate in $PACKAGE's changes"; exit 0; }
fi
count() { n=$(grep -c . "$R/$1" 2>/dev/null); echo "${n:-0}"; }
echo "mutants: $(count caught.txt) caught, $(count missed.txt) missed, $(count timeout.txt) timed out, $(count unviable.txt) unviable"
[ -s "$R/missed.txt" ] && sed 's/^/    missed: /' "$R/missed.txt" | head -20
# cargo-mutants exits 2 when mutants were missed and 3 on timeouts; that's a report, not a failure.
case $code in 0|2|3) exit 0 ;; *) echo "mutants: cargo-mutants failed (exit $code); see .build/mutants/run.log"; tail -5 "$OUT/run.log"; exit 1 ;; esac

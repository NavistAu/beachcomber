#!/usr/bin/env bash
# scripts/conformance-all.sh — local conformance gate.
#
# Builds comb, the cdylib and the Node SDK's dist/ once, then runs every SDK's
# conformance runner against them. Exits non-zero if any build fails, if any
# runner reports a failed fixture, or if a runner could not run at all — a
# runner that never executed must never be counted as a passing one.
#
# Fixtures whose op a binding cannot execute (the Lua runner's subprocess
# transport has no client-side resolver, so it skips "resolve" and "eval")
# are skipped by that runner, not failed, and do not affect this gate's exit
# status — but every runner that skipped anything is named, with counts, in
# the summary.
#
# A skip must still be counted, though: every runner's own pass + fail + skip
# has to add up to the number of fixture files on disk, or this gate fails and
# names it. A runner that returns early from a fixture without counting it, or
# enumerates none at all, otherwise exits 0 and reads as green.
#
# Usage: scripts/conformance-all.sh

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Building comb"
if ! cargo build --bin comb; then
    echo "!!! comb build failed"
    exit 1
fi

echo
echo "==> Building libbeachcomber (cdylib)"
if ! cargo build -p libbeachcomber-ffi; then
    echo "!!! libbeachcomber-ffi build failed"
    exit 1
fi

export COMB_BIN="$ROOT/target/debug/comb"

# Every binding now loads libbeachcomber.{so,dylib} over FFI rather than
# speaking the wire protocol itself (Phase 4 of the client-ABI plan) —
# without this, library discovery falls through to "../lib/ relative to
# comb" and then the platform search path, which won't find a debug build
# that hasn't been packaged anywhere.
case "$(uname -s)" in
    Darwin) export BEACHCOMBER_LIB="$ROOT/target/debug/libbeachcomber.dylib" ;;
    *)      export BEACHCOMBER_LIB="$ROOT/target/debug/libbeachcomber.so" ;;
esac
if [ ! -f "$BEACHCOMBER_LIB" ]; then
    echo "!!! expected library not found: $BEACHCOMBER_LIB"
    exit 1
fi

# The Node runner imports the SDK's compiled dist/, which is not checked in.
# Built here alongside comb and the cdylib rather than left as a manual
# prerequisite: without it the runner exits early on a missing dist/client.js,
# and a runner that never ran must never read as a green one.
echo
echo "==> Building Node SDK (dist/)"
if ! ( cd "$ROOT/sdks/node" && npm ci ); then
    echo "!!! npm ci failed in sdks/node"
    exit 1
fi
if ! ( cd "$ROOT/sdks/node" && npm run build ); then
    echo "!!! Node SDK build failed"
    exit 1
fi

status=0
# Runners that failed or could not run at all, named in the summary so a
# runner that never executed is impossible to mistake for a passing one.
failed_runners=""
# Runners that passed but skipped fixtures, with their counts. A green
# "all SDK runners passed" must not paper over a runner that only ran half
# the suite (the Lua one skips everything its subprocess transport can't do).
skipped_runners=""
# Runners whose own counts do not add up to the fixture total — see the
# completeness check in run_runner.
incomplete_runners=""

# Every fixture on disk. A runner must account for each one as passed, failed
# or skipped; anything else is a fixture that silently never ran.
FIXTURE_TOTAL="$(find "$ROOT/tests/conformance" -name '*.json' | wc -l | tr -d ' ')"
if [ "$FIXTURE_TOTAL" -eq 0 ]; then
    echo "!!! no conformance fixtures found under $ROOT/tests/conformance"
    exit 1
fi

# Extract pass/fail/skip from one runner's summary line into runner_pass,
# runner_fail and runner_skip (empty when the line is absent).
#
# The parse is per-runner because the six summary lines are six different
# shapes — and three of them lead with a denominator that a generic
# "<n> (fixtures )?passed" match captures instead of the numerator, silently
# reporting the suite size as the pass count:
#
#   python  "43 fixtures: 28 passed, 0 failed, 15 skipped"
#   node    "Results: 28/43 passed, 15 skipped."      <- denominator is the total
#   go      "28/28 fixtures passed (15 skipped)"      <- denominator is pass+fail
#   ruby    "Results: 28 passed, 0 failed, 15 skipped out of 43 fixtures"
#   lua     "Results: 28 passed, 0 failed, 15 skipped (of 43 fixtures)"
#   c       "=== Results: 28 passed, 0 failed, 15 skipped ==="
#
# Node prints no failed count and Go prints one only as `denominator - passed`;
# both are reached here solely on a zero exit, where a failed fixture has
# already been caught by the exit-code check above.
parse_counts() {
    parse_name="$1"
    parse_out="$2"
    runner_pass=""
    runner_fail=""
    runner_skip=""
    case "$parse_name" in
        node)
            # Isolate the "<pass>/<total> passed, <skip> skipped" span first: a
            # leading `.*` in the sed would be greedy and could swallow all but
            # the last digit of the pass count.
            parse_line="$(printf '%s' "$parse_out" | grep -oE 'Results: [0-9]+/[0-9]+ passed, [0-9]+ skipped' | tail -1)"
            [ -n "$parse_line" ] || return
            runner_pass="$(printf '%s' "$parse_line" | sed -E 's#^Results: ([0-9]+)/.*#\1#')"
            runner_skip="$(printf '%s' "$parse_line" | sed -E 's#^.* ([0-9]+) skipped$#\1#')"
            runner_fail=0
            ;;
        go)
            parse_line="$(printf '%s' "$parse_out" | grep -oE '[0-9]+/[0-9]+ fixtures passed \([0-9]+ skipped\)' | tail -1)"
            [ -n "$parse_line" ] || return
            runner_pass="$(printf '%s' "$parse_line" | sed -E 's#^([0-9]+)/.*#\1#')"
            parse_denom="$(printf '%s' "$parse_line" | sed -E 's#^[0-9]+/([0-9]+) .*#\1#')"
            runner_skip="$(printf '%s' "$parse_line" | sed -E 's#^.*\(([0-9]+) skipped\)$#\1#')"
            runner_fail=$((parse_denom - runner_pass))
            ;;
        python|ruby|lua|c)
            # All four print "<p> passed, <f> failed, <s> skipped" in that order.
            parse_line="$(printf '%s' "$parse_out" | grep -E '[0-9]+ passed, [0-9]+ failed, [0-9]+ skipped' | tail -1)"
            [ -n "$parse_line" ] || return
            runner_pass="$(printf '%s' "$parse_line" | sed -E 's#.*[^0-9]([0-9]+) passed, [0-9]+ failed, [0-9]+ skipped.*#\1#')"
            runner_fail="$(printf '%s' "$parse_line" | sed -E 's#.*[0-9]+ passed, ([0-9]+) failed, [0-9]+ skipped.*#\1#')"
            runner_skip="$(printf '%s' "$parse_line" | sed -E 's#.*[0-9]+ passed, [0-9]+ failed, ([0-9]+) skipped.*#\1#')"
            ;;
    esac
}

# Run one SDK's conformance runner, echoing its output and checking its counts.
run_runner() {
    runner_name="$1"
    runner_cmd="$2"
    runner_out="$(eval "$runner_cmd" 2>&1)"
    runner_rc=$?
    printf '%s\n' "$runner_out"
    if [ "$runner_rc" -ne 0 ]; then
        status=1
        failed_runners="$failed_runners $runner_name"
        return
    fi
    parse_counts "$runner_name" "$runner_out"
    # A green exit with no parsable summary is the same defect as a runner that
    # never ran: nothing says how much of the suite executed.
    if [ -z "$runner_pass" ] || [ -z "$runner_fail" ] || [ -z "$runner_skip" ]; then
        status=1
        incomplete_runners="$incomplete_runners $runner_name(no-summary)"
        return
    fi
    # Completeness. Exit 0 and "0 failed" is not enough: a runner can return
    # early from a fixture without counting it anywhere (the C runner's
    # `return 1` bail-outs, Ruby's `return unless status_ok`), or enumerate no
    # fixtures at all and exit 0 on the empty set (Python). Each of those reads
    # as a pass today. Every fixture on disk must land in exactly one bucket.
    runner_seen=$((runner_pass + runner_fail + runner_skip))
    if [ "$runner_seen" -ne "$FIXTURE_TOTAL" ]; then
        status=1
        incomplete_runners="$incomplete_runners $runner_name ($runner_seen of $FIXTURE_TOTAL accounted for: ${runner_pass}p/${runner_fail}f/${runner_skip}s)"
    fi
    if [ "$runner_skip" -gt 0 ]; then
        skipped_runners="$skipped_runners $runner_name ($runner_pass pass, $runner_skip skip)"
    fi
}

echo
echo "==> Python SDK conformance"
run_runner python "python3 '$ROOT/sdks/python/conformance_runner.py'"

echo
echo "==> Node.js SDK conformance"
run_runner node "node '$ROOT/sdks/node/conformance_runner.js'"

echo
echo "==> Go SDK conformance"
run_runner go "cd '$ROOT/sdks/go' && go run ./cmd/conformance"

echo
echo "==> Ruby SDK conformance"
run_runner ruby "ruby '$ROOT/sdks/ruby/conformance_runner.rb'"

echo
echo "==> Lua SDK conformance"
run_runner lua "lua '$ROOT/sdks/lua/conformance_runner.lua'"

echo
echo "==> C SDK conformance"
if make -C "$ROOT/sdks/c" conformance >/dev/null; then
    run_runner c "'$ROOT/sdks/c/conformance_runner'"
else
    echo "!!! C SDK conformance runner failed to build"
    status=1
    failed_runners="$failed_runners c(build)"
fi

echo
if [ -n "$skipped_runners" ]; then
    echo "conformance-all: runners with skipped fixtures:$skipped_runners"
fi
if [ -n "$incomplete_runners" ]; then
    echo "conformance-all: runners that did not account for all $FIXTURE_TOTAL fixtures:$incomplete_runners"
fi
if [ -n "$failed_runners" ]; then
    echo "conformance-all: SDK runners failed or could not run:$failed_runners"
fi
if [ "$status" -ne 0 ]; then
    exit 1
fi

echo "conformance-all: all SDK runners passed"

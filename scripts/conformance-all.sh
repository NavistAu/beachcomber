#!/usr/bin/env bash
# scripts/conformance-all.sh — local conformance gate.
#
# Builds comb once, then runs every SDK's conformance runner against it.
# Exits non-zero if the build fails or any runner reports a failed fixture.
#
# Fixtures whose op a binding doesn't implement yet (e.g. "resolve", which
# has no binding until Phase 4) are skipped by each runner, not failed, and
# do not affect this gate's exit status.
#
# Usage: scripts/conformance-all.sh

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Building comb"
if ! cargo build --bin comb; then
    echo "!!! comb build failed"
    exit 1
fi

export COMB_BIN="$ROOT/target/debug/comb"

status=0

echo
echo "==> Python SDK conformance"
python3 "$ROOT/sdks/python/conformance_runner.py" || status=1

echo
echo "==> Node.js SDK conformance"
node "$ROOT/sdks/node/conformance_runner.js" || status=1

echo
echo "==> Go SDK conformance"
( cd "$ROOT/sdks/go" && go run ./cmd/conformance ) || status=1

echo
echo "==> Ruby SDK conformance"
ruby "$ROOT/sdks/ruby/conformance_runner.rb" || status=1

echo
echo "==> Lua SDK conformance"
lua "$ROOT/sdks/lua/conformance_runner.lua" || status=1

echo
echo "==> C SDK conformance"
if make -C "$ROOT/sdks/c" conformance >/dev/null; then
    "$ROOT/sdks/c/conformance_runner" || status=1
else
    echo "!!! C SDK conformance runner failed to build"
    status=1
fi

echo
if [ "$status" -ne 0 ]; then
    echo "conformance-all: one or more SDK runners failed"
    exit 1
fi

echo "conformance-all: all SDK runners passed"

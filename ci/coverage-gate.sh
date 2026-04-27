#!/usr/bin/env bash
set -euo pipefail

BASELINE=${COVERAGE_BASELINE:-66}
echo "Running coverage gate (baseline: ${BASELINE}%)"
cargo llvm-cov nextest \
    -E 'not test(uptime_provider_executes)' \
    --fail-under-lines "${BASELINE}" \
    --summary-only

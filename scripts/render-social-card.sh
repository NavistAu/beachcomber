#!/bin/sh
# render-social-card.sh — rasterise the OG social card SVG to a PNG.
#
# Discord, Slack, and some Twitter/X paths prefer PNG for OG images. The
# canonical source is the SVG (hand-edited with the site's palette); this
# script produces the PNG that docusaurus.config.ts points at.
#
# Usage: scripts/render-social-card.sh
# Requires: node + npx (bundled with Homebrew node).
#
# Re-run whenever the SVG source changes:
#   $ scripts/render-social-card.sh

set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/website/static/img/beachcomber-social-card.svg"
DST="$ROOT/website/static/img/beachcomber-social-card.png"

if [ ! -f "$SRC" ]; then
    echo "source SVG missing: $SRC" >&2
    exit 1
fi

# --yes suppresses the "ok to proceed?" prompt on first use.
npx --yes -p sharp-cli@^2 sharp \
    --input "$SRC" \
    --output "$DST" \
    resize 1200 630 --fit fill

echo "rendered $DST"

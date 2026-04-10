#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-0.3.0}"

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

make -C sdks/c clean all
make -C sdks/c install DESTDIR="$STAGING" PREFIX=/usr VERSION="$VERSION"

mkdir -p "$STAGING/DEBIAN"
cat > "$STAGING/DEBIAN/control" <<EOF
Package: libbeachcomber-dev
Version: $VERSION
Section: libdevel
Priority: optional
Architecture: amd64
Maintainer: NavistAu <github@navistau.io>
Homepage: https://github.com/NavistAu/beachcomber
Description: C client library for the beachcomber daemon (development files)
 Headers, shared library, static library, and pkg-config file for libbeachcomber.
EOF

OUTPUT="libbeachcomber-dev_${VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group "$STAGING" "$OUTPUT"
echo "Built: $OUTPUT"

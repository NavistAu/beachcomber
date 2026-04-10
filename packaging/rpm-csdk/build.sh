#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-0.3.0}"
TOPDIR="$(mktemp -d)"
trap 'rm -rf "$TOPDIR"' EXIT

# Create rpmbuild directory structure
mkdir -p "$TOPDIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Create source tarball from repo root
REPO_ROOT="$(pwd)"
tar czf "$TOPDIR/SOURCES/beachcomber-${VERSION}.tar.gz" \
    --transform "s,^,beachcomber-${VERSION}/," \
    sdks/c/ \
    LICENSE

# Copy spec and build
cp packaging/rpm-csdk/libbeachcomber.spec "$TOPDIR/SPECS/"
rpmbuild --define "_topdir $TOPDIR" \
    --define "version $VERSION" \
    -bb "$TOPDIR/SPECS/libbeachcomber.spec"

# Copy output
cp "$TOPDIR"/RPMS/*/*.rpm .
echo "Built: $(ls ./*.rpm)"

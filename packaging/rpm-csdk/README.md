# rpm-csdk — RPM package for libbeachcomber-devel

Builds an `.rpm` for the C SDK (`libbeachcomber-devel`) using `rpmbuild`.

## Usage

Run from the repository root:

```sh
bash packaging/rpm-csdk/build.sh          # uses default version 0.3.0
bash packaging/rpm-csdk/build.sh 1.2.3    # explicit version
```

The script creates a temporary `rpmbuild` tree, archives `sdks/c/` and the
`LICENSE` file into a source tarball, and calls `rpmbuild -bb` to produce an
`.rpm` in the current directory.

Requires: `gcc`, `make`, and `rpmbuild` (available in `rpm-build` on Fedora/RHEL/openSUSE).

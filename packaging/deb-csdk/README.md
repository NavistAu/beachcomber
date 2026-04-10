# deb-csdk — Debian package for libbeachcomber-dev

Builds a `.deb` for the C SDK (`libbeachcomber-dev`) using `dpkg-deb`.

## Usage

Run from the repository root:

```sh
bash packaging/deb-csdk/build.sh          # uses default version 0.3.0
bash packaging/deb-csdk/build.sh 1.2.3    # explicit version
```

The script builds the C SDK with `make`, stages the install under a temp
directory, writes a `DEBIAN/control` file, and calls `dpkg-deb` to produce
`libbeachcomber-dev_<VERSION>_amd64.deb` in the current directory.

Requires: `gcc`, `make`, and `dpkg-deb` (available in `dpkg-dev` on Debian/Ubuntu).

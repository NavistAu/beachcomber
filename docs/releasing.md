# Release Checklist

Step-by-step process for cutting a new beachcomber release.

## Prerequisites

- All feature work merged to `main`
- No outstanding branches or worktrees (`git worktree list`, `git branch -a`)

## 1. Version Bump

Update version in **all** of these files:

| File | Field |
|---|---|
| `Cargo.toml` | `version` |
| `beachcomber-client/Cargo.toml` | `version` |
| `sdks/node/package.json` | `"version"` |
| `sdks/python/pyproject.toml` | `version` |
| `sdks/ruby/libbeachcomber.gemspec` | `s.version` |
| `sdks/lua/rockspec/libbeachcomber-X.Y.Z-1.rockspec` | `version`, `tag` (rename the file too) |
| `packaging/aur/beachcomber/PKGBUILD` | `pkgver` |
| `packaging/aur/beachcomber-bin/PKGBUILD` | `pkgver` |
| `packaging/aur/libbeachcomber/PKGBUILD` | `pkgver` |
| `packaging/nix/flake.nix` | `version` |
| `README.md` | `.deb` and `.rpm` download URLs in install section |
| `.github/workflows/release.yml` | LuaRocks rockspec filename in `publish-luarocks` job |

After editing, run `cargo check` to regenerate `Cargo.lock`.

Remove the old Lua rockspec file (`git rm sdks/lua/rockspec/libbeachcomber-OLD-1.rockspec`).

## 2. Changelog

Add a new `## [X.Y.Z] - YYYY-MM-DD` section at the top of `CHANGELOG.md` following Keep a Changelog format. Sections: Added, Changed, Fixed, Removed (as applicable).

## 3. Documentation

Verify docs reflect the new version's features:

- `README.md` — CLI reference, protocol ops, feature descriptions
- `CLAUDE.md` — protocol ops list, architecture notes
- `docs/architecture.md` — design decisions
- `docs/roadmap.md` — mark shipped items as `[x]`
- `website/docs/reference/cli-commands.md` — new subcommands
- `website/docs/reference/protocol-reference.md` — new protocol ops

## 4. CI Green

Commit, push to `main`, and wait for **all CI jobs** to pass:

- Check (macOS + Linux): cargo check, clippy, fmt
- Test (macOS + Linux): cargo test
- Benchmark
- SDK tests: C, Go, Lua, Node.js, Python, Ruby
- Installer validation: npm, PyPI

Do not proceed until CI is fully green.

## 5. Tag and Release

```sh
git tag v0.X.0
git push origin v0.X.0
```

The `v*` tag triggers `.github/workflows/release.yml` which:

1. Builds binaries for macOS (aarch64, x86_64) and Linux (x86_64 gnu, x86_64 musl)
2. Packages C SDK tarball
3. Builds .deb and .rpm packages for the daemon, smoke-tests them in containers
4. Builds .deb and .rpm packages for the C SDK (`libbeachcomber-dev` / `libbeachcomber-devel`), smoke-tests them
5. Creates a GitHub Release with all artifacts
6. Publishes to: crates.io, PyPI, npm, RubyGems, LuaRocks
7. Tags the Go module (`sdks/go/vX.Y.Z`)
8. Updates the Homebrew tap formula
9. Publishes binary installer packages to npm and PyPI

## 6. Verify Publish

After the release workflow completes, verify packages are live:

- **GitHub Release:** `gh release view vX.Y.Z --repo NavistAu/beachcomber`
- **crates.io:** `cargo search beachcomber`
- **npm:** `npm view libbeachcomber version` and `npm view beachcomber version`
- **PyPI:** `pip index versions libbeachcomber` and `pip index versions beachcomber`
- **RubyGems:** `gem search libbeachcomber`
- **LuaRocks:** `luarocks search libbeachcomber`
- **Homebrew:** `brew info NavistAu/tap/beachcomber`

## 7. Post-Release

- Verify website deployed (auto-triggers on push to `main`)
- Check the release workflow run for any publish failures: `gh run view <id> --repo NavistAu/beachcomber`
- If any publish job failed, it can usually be re-run individually from the GitHub Actions UI

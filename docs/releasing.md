# Release Checklist

Step-by-step process for cutting a new beachcomber release.

> **Branch model:** `develop` is the default/integration branch; `main` is the release branch.
> Releases are prepared on `develop`, promoted to `main` via a PR (the release gate — `main` is
> protected and requires green CI to merge), and tagged on `main`. Tags are only ever cut on `main`.
> See `CONTRIBUTING.md` → "Branch Workflow".

## Prerequisites

- All feature work for the release merged to `develop`, and `develop` is green
- No outstanding feature branches or worktrees (`git worktree list`, `git branch -a`)
- Version bump and changelog (steps 1–3) are done **on `develop`**

## 1. Version Bump

One command rewrites every version touchpoint:

```sh
cargo xtask set-version X.Y.Z          # preview first with --dry-run
```

This updates all 14 files — `Cargo.toml`, `beachcomber-client/Cargo.toml`, both lockfiles, the 5 SDK manifests, the 3 AUR PKGBUILDs, `packaging/nix/flake.nix`, the `release.yml` rockspec reference, the README `.deb`/`.rpm` download URLs, and the Lua rockspec (renaming its versioned filename and updating `version` + `tag`) — then runs `cargo check` to validate the workspace and refresh `Cargo.lock`. Each edit is count-guarded: if any file's occurrence count has drifted (e.g. a manifest was reformatted) the run aborts before writing anything, so re-sync that file and re-run. Use `--dry-run` to preview the plan and `--no-verify` to skip the `cargo check`.

The CHANGELOG is intentionally **not** touched — write release notes by hand (next step).

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

## 4. Release PR (`develop` → `main`) — the release gate

Commit steps 1–3 to `develop` and push it. Then open a PR from `develop` into `main`:

```sh
git push origin develop
gh pr create --base main --head develop --title "Release vX.Y.Z" --fill
```

`main` is protected: the PR cannot merge until **all CI jobs** pass:

- Check (macOS + Linux): cargo check, clippy, fmt
- Test (macOS + Linux): cargo nextest run
- Benchmark
- SDK tests: C, Go, Lua, Node.js, Python, Ruby
- Installer validation: npm, PyPI

Do not merge until CI is fully green. Merging the PR is the act of promoting the release to `main`.

## 5. Tag and Release (on `main`)

After the release PR merges, tag the new `main` HEAD. Tags are only ever created on `main`:

```sh
git checkout main && git pull
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

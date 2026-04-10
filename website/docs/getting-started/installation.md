---
sidebar_position: 1
---

# Installation

## Homebrew

```sh
brew install navistau/tap/beachcomber
```

## npm

```sh
npm install -g beachcomber
```

Or run without installing:

```sh
npx beachcomber --version
```

## pip

```sh
pip install beachcomber
```

Or with uv:

```sh
uv tool install beachcomber
# or run without installing:
uvx beachcomber --version
```

The npm and pip packages download the correct pre-built binary for your platform from GitHub Releases. Once installed, the binary persists independently of the package manager.

## Cargo

```sh
cargo install beachcomber
```

## Debian/Ubuntu

Download the `.deb` from the latest release:

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/beachcomber_0.4.0-1_amd64.deb
sudo dpkg -i beachcomber_0.4.0-1_amd64.deb
```

```sh
# ARM64
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/beachcomber_0.4.0-1_arm64.deb
sudo dpkg -i beachcomber_0.4.0-1_arm64.deb
```

## Fedora/RHEL

Download the `.rpm` from the latest release:

```sh
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/beachcomber-0.4.0-1.x86_64.rpm
sudo rpm -i beachcomber-0.4.0-1.x86_64.rpm
```

```sh
# ARM64
curl -LO https://github.com/NavistAu/beachcomber/releases/latest/download/beachcomber-0.4.0-1.aarch64.rpm
sudo rpm -i beachcomber-0.4.0-1.aarch64.rpm
```

## Arch Linux (AUR)

```sh
# From source
yay -S beachcomber

# Prebuilt binary
yay -S beachcomber-bin
```

## Nix

```sh
nix run github:NavistAu/beachcomber
```

## Pre-built binaries

Pre-built binaries are available on the [GitHub Releases](https://github.com/NavistAu/beachcomber/releases) page for the following targets:

- `aarch64-apple-darwin` (macOS, Apple Silicon)
- `x86_64-apple-darwin` (macOS, Intel)
- `x86_64-unknown-linux-gnu` (Linux, glibc)
- `x86_64-unknown-linux-musl` (Linux, musl/static)
- `aarch64-unknown-linux-gnu` (Linux ARM64, glibc)

# beachcomber

Install the [beachcomber](https://beachcomber.sh) (`comb`) shell-state daemon.

This package downloads the correct pre-built binary for your platform from GitHub Releases. It does not contain the binary itself.

## Install

```sh
pip install beachcomber
# or
uv tool install beachcomber
# or run without installing:
uvx beachcomber --version
```

The binary is installed to `~/.local/bin/comb`. Once installed, you can uninstall this Python package — the binary remains.

## Client SDK

The beachcomber **client SDK** is a separate package: `pip install libbeachcomber`.

- Website: https://beachcomber.sh
- GitHub: https://github.com/NavistAu/beachcomber

"""beachcomber binary installer — downloads comb from GitHub Releases."""

import os
import platform
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from importlib.metadata import version as pkg_version
from pathlib import Path

PLATFORM_MAP = {
    ("darwin", "arm64"): "aarch64-apple-darwin",
    ("darwin", "x86_64"): "x86_64-apple-darwin",
    ("linux", "x86_64"): "x86_64-unknown-linux-musl",
}

INSTALL_DIR = Path.home() / ".local" / "bin"

URL_TEMPLATE = (
    "https://github.com/NavistAu/beachcomber/releases/download/"
    "v{version}/beachcomber-v{version}-{target}.tar.gz"
)


def _get_version():
    return pkg_version("beachcomber")


def _get_target():
    key = (sys.platform, platform.machine())
    target = PLATFORM_MAP.get(key)
    if not target:
        print(
            f"Error: beachcomber does not provide a pre-built binary "
            f"for {sys.platform} {platform.machine()}.\n\n"
            f"Install from source:  cargo install beachcomber\n"
            f"Supported platforms:  macOS (arm64, x86_64), Linux (x86_64)\n"
            f"More info:            https://beachcomber.sh",
            file=sys.stderr,
        )
        sys.exit(1)
    return target


def _installed_version(binary_path):
    """Return the version string from an existing comb binary, or None."""
    try:
        result = subprocess.run(
            [str(binary_path), "--version"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        # Output is "comb X.Y.Z"
        if result.returncode == 0:
            parts = result.stdout.strip().split()
            if len(parts) >= 2:
                return parts[1]
    except (OSError, subprocess.TimeoutExpired):
        pass
    return None


def _download(version, target):
    """Download and extract comb binary to INSTALL_DIR."""
    url = URL_TEMPLATE.format(version=version, target=target)
    print(f"Downloading comb v{version} for {target}...", file=sys.stderr)

    INSTALL_DIR.mkdir(parents=True, exist_ok=True)

    try:
        with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
            tmp_path = tmp.name
            urllib.request.urlretrieve(url, tmp_path)
    except Exception as exc:
        print(
            f"Error: Failed to download comb binary from GitHub Releases.\n\n"
            f"URL: {url}\n"
            f"{exc}\n\n"
            f"Try installing manually:\n"
            f"  brew install navistau/tap/beachcomber\n"
            f"  cargo install beachcomber\n\n"
            f"More info: https://beachcomber.sh",
            file=sys.stderr,
        )
        sys.exit(1)

    try:
        with tarfile.open(tmp_path, "r:gz") as tar:
            member = tar.getmember("comb")
            member.name = "comb"  # strip any path prefix
            tar.extract(member, path=str(INSTALL_DIR))
    except Exception as exc:
        print(
            f"Error: Failed to extract comb binary from tarball.\n{exc}",
            file=sys.stderr,
        )
        sys.exit(1)
    finally:
        os.unlink(tmp_path)

    binary_path = INSTALL_DIR / "comb"
    os.chmod(binary_path, 0o755)

    print(f"Installed comb v{version} to {binary_path}", file=sys.stderr)
    print(
        "You can now run comb directly. "
        "To remove the Python installer: pip uninstall beachcomber",
        file=sys.stderr,
    )
    return binary_path


def main():
    version = _get_version()
    binary_path = INSTALL_DIR / "comb"

    # Fast path: binary exists and version matches
    if binary_path.is_file():
        installed = _installed_version(binary_path)
        if installed == version:
            os.execv(str(binary_path), [str(binary_path)] + sys.argv[1:])

    # Download the binary
    target = _get_target()
    binary_path = _download(version, target)

    # Exec into the downloaded binary
    os.execv(str(binary_path), [str(binary_path)] + sys.argv[1:])

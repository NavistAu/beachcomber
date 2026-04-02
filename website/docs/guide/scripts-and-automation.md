---
sidebar_position: 7
---

# Scripts and Automation

## Python script

The `beachcomber` Python SDK is stdlib-only (no pip dependencies required):

```python
from beachcomber import Client

client = Client()

# Single field
result = client.get("git.branch", path="/path/to/repo")
if result.is_hit:
    print(f"Branch: {result.data}")

# Full provider with field access
result = client.get("git", path="/path/to/repo")
if result.is_hit:
    print(f"Branch: {result['branch']}, dirty: {result['dirty']}")

# Persistent session for multiple queries
with client.session() as s:
    s.set_context("/path/to/repo")
    branch = s.get("git.branch")
    battery = s.get("battery.percent")
```

Or connect directly with no SDK — the protocol is newline-delimited JSON over a Unix socket.

## Shell one-liner for scripts and CI

For scripts that want to annotate output with git context but don't require beachcomber to be installed:

```sh
# Returns branch name — uses beachcomber if available, falls back to git
BRANCH=$(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")

# In CI, log the current branch alongside build output
echo "Building branch: $(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD)"

# Check if repo is dirty before deploying
if [ "$(comb get git.dirty . -f text 2>/dev/null)" = "true" ]; then
    echo "Warning: uncommitted changes"
fi
```

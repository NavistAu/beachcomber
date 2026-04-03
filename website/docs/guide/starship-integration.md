---
sidebar_position: 5
---

# Starship Integration

starship's `[custom.*]` modules run a shell command and display its output. Using beachcomber as the backend replaces starship's per-prompt git computation with a cache read.

```toml
# ~/.config/starship.toml

# Replace starship's built-in git_branch with a beachcomber-backed one
[git_branch]
disabled = true

[custom.git_branch]
command = "comb get git.branch . -f text"
when = "comb get git.branch . -f text"
format = "[$output]($style) "
style = "bold blue"
description = "Git branch via beachcomber"

[custom.git_dirty]
command = 'test "$(comb get git.dirty . -f text)" = "true" && echo "*"'
when = "comb get git.dirty . -f text"
format = "[$output]($style)"
style = "bold red"

[custom.kube]
command = "comb get kubecontext.context -f text"
when = "comb get kubecontext.context -f text"
format = "[$output]($style) "
style = "bold cyan"
symbol = "☸ "
```

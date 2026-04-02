---
sidebar_position: 3
---

# tmux Integration

tmux evaluates `#(command)` format strings to populate the status bar. Each `#()` is a subprocess — beachcomber makes these essentially free because the daemon is already running.

```tmux
# ~/.tmux.conf

# Battery percentage and git branch in right status
set -g status-right '#(comb get battery.percent -f text)%% bat | #(comb get git.branch . -f text)'

# Left: session name + kubernetes context
set -g status-left '[#S] #(comb get kubecontext.context -f text)'

# Refresh interval — lower is fine because queries cost almost nothing
set -g status-interval 5
```

**Why this is different from the problem described above:** each `#()` invocation still forks a shell, but `comb` reads a pre-cached value in ~34µs instead of spawning git (5ms+) or running a battery subprocess (6ms). The total time savings across a 50-pane tmux session is substantial.

The simple `#()` approach shown above is already a major improvement over shelling out to git or battery commands directly. Each `comb get` also signals demand to the daemon, keeping the provider warm automatically.

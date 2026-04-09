---
sidebar_position: 11
---

# Oh My Posh Integration

## Introduction

oh-my-posh is a cross-shell prompt theme engine supporting bash, zsh, fish, and PowerShell. It supports custom segments that run external commands. beachcomber replaces the per-prompt subprocess cost with cached reads.

## Prerequisites

- beachcomber installed and daemon running. Verify with:

```sh
comb get load.one
```

If this returns a value, the daemon is up. If it returns an error, start the daemon with `comb start` or check your shell's startup file.

- oh-my-posh installed. Verify with:

```sh
oh-my-posh --version
```

## Configuration

oh-my-posh uses a JSON, YAML, or TOML config file. Custom segments use the `command` type with a `shell` and `command` property. beachcomber's `comb get` command returns a single line of output (or nothing), which maps cleanly to oh-my-posh's `{{ .Output }}` template variable.

### Example theme (TOML)

Create or edit your theme file (e.g., `~/.config/oh-my-posh/beachcomber.toml`):

```toml
"$schema" = "https://raw.githubusercontent.com/JanDeDobbeleer/oh-my-posh/main/themes/schema.json"
version = 2
final_space = true

[[blocks]]
type = "prompt"
alignment = "left"

    [[blocks.segments]]
    type = "path"
    style = "plain"
    foreground = "green"
    template = "{{ .Path }} "

    [[blocks.segments]]
    type = "command"
    style = "plain"
    foreground = "blue"
        [blocks.segments.properties]
        shell = "bash"
        command = "comb get git.branch . -f text 2>/dev/null"
    template = "{{ if .Output }} {{ .Output }}{{ end }}"

    [[blocks.segments]]
    type = "command"
    style = "plain"
    foreground = "red"
        [blocks.segments.properties]
        shell = "bash"
        command = "test \"$(comb get git.dirty . -f text 2>/dev/null)\" = \"true\" && echo \"*\""
    template = "{{ .Output }}"

    [[blocks.segments]]
    type = "text"
    style = "plain"
    foreground = "white"
    template = "> "

[[blocks]]
type = "rprompt"
alignment = "right"

    [[blocks.segments]]
    type = "command"
    style = "plain"
    foreground = "cyan"
        [blocks.segments.properties]
        shell = "bash"
        command = "comb get kubecontext.context -f text 2>/dev/null"
    template = "{{ if .Output }}☸ {{ .Output }} {{ end }}"

    [[blocks.segments]]
    type = "command"
    style = "plain"
    foreground = "yellow"
        [blocks.segments.properties]
        shell = "bash"
        command = "comb get aws.profile -f text 2>/dev/null"
    template = "{{ if .Output }} {{ .Output }} {{ end }}"

    [[blocks.segments]]
    type = "command"
    style = "plain"
    foreground = "green"
        [blocks.segments.properties]
        shell = "bash"
        command = "comb get battery.percent -f text 2>/dev/null"
    template = "{{ if .Output }} {{ .Output }}%{{ end }}"
```

### Activating the theme

Add the appropriate line to your shell's startup file:

```sh
# bash (~/.bashrc)
eval "$(oh-my-posh init bash --config ~/.config/oh-my-posh/beachcomber.toml)"

# zsh (~/.zshrc)
eval "$(oh-my-posh init zsh --config ~/.config/oh-my-posh/beachcomber.toml)"

# fish (~/.config/fish/config.fish)
oh-my-posh init fish --config ~/.config/oh-my-posh/beachcomber.toml | source
```

## Troubleshooting

- **Segment not rendering:** oh-my-posh hides command segments with empty output. Verify `comb get <key> -f text` returns data in your terminal before concluding the theme is misconfigured.
- **Wrong directory for git:** oh-my-posh runs command segments in the shell's working directory, so `.` resolves correctly without any extra configuration.

See the [Troubleshooting](./troubleshooting.md) guide for general beachcomber diagnostics.

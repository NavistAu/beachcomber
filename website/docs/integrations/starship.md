---
sidebar_position: 3
---

# Starship

Starship is a fast, cross-shell prompt with over 55,000 GitHub stars. It ships with built-in modules for git, kubernetes, cloud providers, language environments, and more — but every one of those modules runs a fresh process on each prompt render. In a typical developer environment with git status, kubectl context, AWS profile, and a language version check all enabled, you are spawning five or more subprocesses before your prompt appears.

Beachcomber eliminates that overhead. Its `comb g` command reads from an in-memory cache maintained by a background daemon. The first call populates the cache; every subsequent call returns in microseconds. Starship's `[custom.*]` module system is a perfect fit: point the `command` and `when` fields at `comb g`, disable the conflicting built-in module, and your prompt becomes a cache read.

## Prerequisites

- Beachcomber is installed. Verify the daemon is reachable with:

```sh
comb g load.one
```

If this prints a number, the daemon is up. The daemon is socket-activated — it starts on the first `comb` call and shuts down after an idle period. You do not need to launch it manually. If the command errors, confirm `comb --version` works and see the [Debugging](../about/debugging.md) page.

- Starship is installed and active in your shell. Verify with:

```sh
starship --version
```

- The `comb` binary is on your `PATH` so starship can invoke it from a non-interactive shell context. Verify with:

```sh
which comb
```

## How custom modules work

Starship's `[custom.NAME]` blocks define a module that runs an arbitrary shell command. Two fields matter here:

- `command` — the shell command whose stdout becomes the module's displayed text.
- `when` — a shell command that must exit 0 and produce non-empty output for the module to appear. If the `when` command returns empty output, starship hides the module entirely.

For beachcomber, both fields can use the same `comb g` invocation. When the daemon has no data for a key (daemon not running, provider not applicable to the current directory), `comb g` returns empty output. That naturally satisfies the `when` contract — if there is no data, the module disappears cleanly.

## Where to put the config

Starship reads from `~/.config/starship.toml`. Open that file and add the custom module blocks shown in the sections below. If the file does not exist yet, create it.

To disable a built-in starship module that conflicts with your custom one, add the corresponding section with `disabled = true`. For example, to replace the built-in git branch display:

```toml
[git_branch]
disabled = true
```

## Testing that it works

After editing `~/.config/starship.toml`, test any module directly from the terminal before opening a new shell:

```sh
comb g git.branch .    # g = get, text is the default format
```

Expected output: the current branch name, e.g. `main`.

Then check that starship renders the module:

```sh
starship module custom.git_branch
```

If the module is configured correctly and `comb g` returns data, this prints the formatted module output. If it prints nothing, the `when` condition evaluated to empty — see the Troubleshooting section.

## Git modules

Disable the built-in git modules and replace them with cache-backed equivalents:

```toml
[git_branch]
disabled = true

[git_status]
disabled = true

[custom.git_branch]
command = "comb g git.branch ."
when = "comb g git.branch ."
format = "[$output]($style) "
style = "bold blue"
description = "Git branch via beachcomber"

[custom.git_dirty]
command = 'test "$(comb g git.dirty .)" = "true" && echo "*"'
when = "comb g git.branch ."
format = "[$output]($style)"
style = "bold red"
description = "Git dirty indicator via beachcomber"

[custom.git_stash]
command = "comb g git.stash_count ."
when = "comb g git.stash_count ."
format = '[stash:$output]($style) '
style = "yellow"
description = "Git stash count via beachcomber"
```

The `git_dirty` module uses a small shell expression because the raw value is `true` or `false` — the test converts it to a displayable `*` character or suppresses output entirely.

Path-scoped providers (git, terraform, mise, asdf) require the current directory to be passed as the second argument. The `.` shown above tells starship to pass `$PWD` at render time. Omitting it causes beachcomber to look up data with no path context, which returns empty for path-scoped providers.

## System modules

These providers are global — no path argument is needed.

```toml
[custom.battery]
command = "comb g battery.percent"
when = "comb g battery.percent"
format = "[BAT $output%]($style) "
style = "bold yellow"
description = "Battery percentage via beachcomber"

[custom.load]
command = "comb g load.one"
when = "comb g load.one"
format = "[load $output]($style) "
style = "dimmed white"
description = "1-minute load average via beachcomber"

[custom.network]
command = "comb g network.ssid"
when = "comb g network.ssid"
format = "[$output]($style) "
style = "dimmed cyan"
description = "Wi-Fi SSID via beachcomber"
```

## Kubernetes module

Replace starship's built-in kubernetes module, which calls `kubectl` on every prompt:

```toml
[kubernetes]
disabled = true

[custom.kube]
command = "comb g kubecontext.context"
when = "comb g kubecontext.context"
format = "[$output]($style) "
style = "bold cyan"
description = "Kubernetes context via beachcomber"

[custom.kube_namespace]
command = "comb g kubecontext.namespace"
when = "comb g kubecontext.namespace"
format = "[$output]($style) "
style = "cyan"
description = "Kubernetes namespace via beachcomber"
```

## Cloud provider modules

```toml
[aws]
disabled = true

[custom.aws]
command = "comb g aws.profile"
when = "comb g aws.profile"
format = "[aws:$output]($style) "
style = "bold orange"
description = "AWS profile via beachcomber"

[gcloud]
disabled = true

[custom.gcloud]
command = "comb g gcloud.project"
when = "comb g gcloud.project"
format = "[gcp:$output]($style) "
style = "bold blue"
description = "GCloud project via beachcomber"
```

## Environment and toolchain modules

`terraform`, `mise`, and `asdf` are path-scoped — pass `.` so beachcomber receives the current directory. `conda` is a global provider and does not require a path argument.

```toml
[conda]
disabled = true

[custom.conda]
command = "comb g conda.env"
when = "comb g conda.env"
format = "[conda:$output]($style) "
style = "bold green"
description = "Conda environment via beachcomber (global provider)"

[terraform]
disabled = true

[custom.terraform]
command = "comb g terraform.workspace ."
when = "comb g terraform.workspace ."
format = "[tf:$output]($style) "
style = "bold purple"
description = "Terraform workspace via beachcomber"
```

## Complete example

A realistic config combining several modules. Paste this into `~/.config/starship.toml` and adjust styles to taste:

```toml
# Disable built-in modules replaced by beachcomber
[git_branch]
disabled = true

[git_status]
disabled = true

[kubernetes]
disabled = true

[aws]
disabled = true

[gcloud]
disabled = true

[conda]
disabled = true

[terraform]
disabled = true

# Beachcomber-backed modules

[custom.git_branch]
command = "comb g git.branch ."
when = "comb g git.branch ."
format = "on [$output]($style) "
style = "bold blue"
description = "Git branch via beachcomber"

[custom.git_dirty]
command = 'test "$(comb g git.dirty .)" = "true" && echo "*"'
when = "comb g git.branch ."
format = "[$output]($style)"
style = "bold red"
description = "Git dirty indicator via beachcomber"

[custom.kube]
command = "comb g kubecontext.context"
when = "comb g kubecontext.context"
format = "[kube:$output]($style) "
style = "bold cyan"
description = "Kubernetes context via beachcomber"

[custom.aws]
command = "comb g aws.profile"
when = "comb g aws.profile"
format = "[aws:$output]($style) "
style = "bold yellow"
description = "AWS profile via beachcomber"

[custom.gcloud]
command = "comb g gcloud.project"
when = "comb g gcloud.project"
format = "[gcp:$output]($style) "
style = "bold blue"
description = "GCloud project via beachcomber"

[custom.terraform]
command = "comb g terraform.workspace ."
when = "comb g terraform.workspace ."
format = "[tf:$output]($style) "
style = "bold purple"
description = "Terraform workspace via beachcomber"

[custom.conda]
command = "comb g conda.env"
when = "comb g conda.env"
format = "[conda:$output]($style) "
style = "bold green"
description = "Conda environment via beachcomber (global provider)"

[custom.load]
command = "comb g load.one"
when = "comb g load.one"
format = "[load:$output]($style) "
style = "dimmed white"
description = "System load via beachcomber"

[custom.battery]
command = "comb g battery.percent"
when = "comb g battery.percent"
format = "[bat:$output%]($style) "
style = "bold yellow"
description = "Battery via beachcomber"
```

## Expected output

With the complete config active, a prompt in a git repository checked out to a feature branch, connected to an AWS account, with a kubernetes context selected, looks something like:

```
~/projects/myapp on main * kube:prod-us-east-1 aws:engineering load:1.42
>
```

Modules that have no data for the current context are suppressed automatically. In a directory with no git repository, `custom.git_branch` and `custom.git_dirty` do not appear. On a machine with no kubeconfig, `custom.kube` is hidden. No configuration changes are needed — the `when` field handles it.

## Troubleshooting

- **Modules hidden when they should show:** starship runs `command` and `when` in a non-interactive shell. If `comb` is only on your interactive shell's PATH, starship will not find it. See [comb not found in non-interactive shells](./troubleshooting.md#comb-not-found-in-non-interactive-shells).
- **Module shows empty outside a repo:** this is correct. Path-scoped providers return empty when the directory is not a relevant context. The `when` field hides the module automatically.
- **Testing a module in isolation:** `starship module custom.git_branch` renders just that module using your current config and directory.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.

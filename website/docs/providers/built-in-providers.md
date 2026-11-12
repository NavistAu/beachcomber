---
sidebar_position: 2
title: Built-in Providers
---

# Built-in Providers

Everything on this page ships with comb and works with no setup. Each namespace is ordinary configuration in the
managed `~/.config/beachcomber/defaults/` directory, which you can retune or switch off like any other; see
[Configuration](../concepts/configuration.md), and [Custom Providers](../custom-providers/overview.md) to add your own.

Every example below is a real run, with personal details replaced.

## Reading this page

- **Scope.** A *global* namespace has one answer for the whole machine, whatever directory you ask from. A
  *directory* namespace answers for the project around the directory you ask from (the repository, the Python
  project, …), so `comb g git.branch` gives a different answer in each repository and nothing outside one.
- **Cached or rendered.** Most namespaces are computed by the daemon and cached. A few are templates your own shell
  renders on each read, from its environment, so they follow `export AWS_PROFILE=…` at once. See
  [The Tree](../concepts/values-and-names.md).
- **Absent fields.** A field with no value is left out, never `""`. `comb g` prints nothing for it and `comb g.j`
  prints `null`.

## At a glance

| Namespace | Tells you | Scope | Needs |
|---|---|---|---|
| [`hostname`](#hostname) | Machine name | global | — |
| [`user`](#user) | Login name and uid | global | — |
| [`uname`](#uname) | Kernel and architecture | global | — |
| [`load`](#load) | Load averages | global | — |
| [`uptime`](#uptime) | Time since boot | global | — |
| [`battery`](#battery) | Charge and charging state | global | a battery |
| [`network`](#network) | Interface, addresses, VPN, Wi-Fi | global | Linux SSID: `nmcli` or `iw` |
| [`sudo`](#sudo) | Whether sudo is cached | global | Linux only, in practice |
| [`git`](#git) | Branch, status, divergence | directory | `git` |
| [`python`](#python) | Virtualenv and interpreter version | directory | — |
| [`mise`](#mise) | Tool versions mise has in effect | directory | `mise` |
| [`asdf`](#asdf) | Tool versions from `.tool-versions` | directory | — |
| [`direnv`](#direnv) | Whether the `.envrc` is allowed | directory | — |
| [`terraform`](#terraform) | Selected workspace | directory | — |
| [`aws`](#aws), [`aws_profiles`](#aws_profiles) | Profile and region in effect; every profile's region | global | — |
| [`gcloud`](#gcloud), [`gcloud_configs`](#gcloud_configs) | Project and account in effect; every configuration | global | — |
| [`kubecontext`](#kubecontext) | kubectl context and namespace | global | `kubectl` |
| [`talos`](#talos) | talosctl context | global | `talosctl` |
| [`docker`](#docker), [`podman`](#podman) | Context, whether the engine answers, running containers | global | `docker` / `podman` |
| [`containers`](#containers) | Whichever engine is running, for themes written for everyone | global | either |
| [`brew`](#brew), [`apt`](#apt), [`dnf`](#dnf), [`pacman`](#pacman), [`apk`](#apk) | Outdated packages, from what the manager already knows | global | the manager |
| [`conda`](#conda) | Active conda environment | global | — |
| [`op`](#op) | Whether a 1Password service-account token is set | global | — |
| [`ssh`](#ssh) | Whether your ssh agent answers, and the keys it holds | global | `ssh-add` |
| [`gpg`](#gpg) | Whether gpg-agent runs, and how many keys have a cached passphrase | global | `gpg-connect-agent` |
| [`kerberos`](#kerberos) | Whether you hold a valid Kerberos ticket | global | `klist` |

"Needs" is what must be installed for the namespace to have values. A tool the daemon runs (`git`, `mise`) must be on
the daemon's `PATH`, which is the `PATH` of the shell that started it. A tool a template runs (`kubectl`, `talosctl`)
must be on your shell's `PATH`.

## System

These read the operating system directly through the `sys` plugin compiled into comb. Only two spawn a process:
`battery` on macOS (`pmset`) and the Wi-Fi name in `network`.

### hostname

The machine's name. Polled every 10 minutes while read.

| Field | Meaning | Example |
|---|---|---|
| `name` | Full hostname | `laptop.local` |
| `short` | The hostname up to its first `.` | `laptop` |

### user

The user the daemon runs as. Polled every 10 minutes.

| Field | Meaning | Example |
|---|---|---|
| `name` | Login name; absent for a uid with no account entry | `jane` |
| `uid` | Numeric user id | `501` |

### uname

The kernel, as `uname` reports it. Polled hourly.

| Field | Meaning | Example |
|---|---|---|
| `sysname` | Kernel name | `Darwin`, `Linux` |
| `release` | Kernel release | `24.6.0` |
| `version` | Kernel version string | `Darwin Kernel Version 24.6.0: …` |
| `machine` | Hardware architecture | `arm64`, `x86_64` |

### load

The 1, 5 and 15 minute load averages. Polled every 10 seconds.

| Field | Meaning | Example |
|---|---|---|
| `one` | 1-minute average | `2.3916015625` |
| `five` | 5-minute average | `1.873046875` |
| `fifteen` | 15-minute average | `1.4189453125` |

The values are unrounded; round them in a template (`{{ load.one | round(1) }}`).

### uptime

Time since boot. Polled every minute. `days`, `hours` and `minutes` split `seconds` up, so the uptime is
`days`d `hours`h `minutes`m.

| Field | Meaning | Example |
|---|---|---|
| `seconds` | Seconds since boot | `4529149` |
| `days` | Whole days | `52` |
| `hours` | Hours past the last whole day (0–23) | `10` |
| `minutes` | Minutes past the last whole hour (0–59) | `5` |

### battery

The battery's charge. Polled every 30 seconds. macOS reads `pmset -g batt`; Linux reads the first battery under
`/sys/class/power_supply`. A machine with no battery has no `battery` fields.

| Field | Meaning | Example |
|---|---|---|
| `percent` | Charge, 0–100 | `80` |
| `charging` | Whether it is charging | `false` |
| `status` | `charging`, `discharging`, `charged`, `calculating` or `unknown` | `discharging` |
| `time_remaining_secs` | Estimated seconds to empty or full; absent when there is no estimate | `7200` |
| `status_raw` | Linux only: the kernel's own status word | `Discharging` |

### network

The network connection. Polled every 10 seconds.

| Field | Meaning | Example |
|---|---|---|
| `interface` | The preferred interface: an `en*` one on macOS, the default route's on Linux | `en0` |
| `ip` | Its IPv4 address | `192.168.1.42` |
| `ipv6` | The first routable IPv6 address | `fd00::1c87:ef81:7850:3561` |
| `vpn_active` | Whether a VPN interface is up | `true` |
| `vpn_name` | The first VPN interface | `utun0` |
| `ssid` | Wi-Fi network name; absent on macOS 14.4 and later, which removed the `airport` tool | `HomeNet` |
| `online` | Whether there is an address at all | `true` |

On Linux the SSID comes from `nmcli`, else `iw`.

### sudo

Whether `sudo` would run without asking for a password: the user's sudo timestamp is under five minutes old. Polled
every 30 seconds.

| Field | Meaning | Example |
|---|---|---|
| `active` | Whether sudo is cached | `true` |

It reads `/var/run/sudo/ts/$USER`, else `/run/sudo/ts/$USER`. Where that directory is readable only by root (many
Linux systems), and always on macOS, which keeps its timestamps under `/var/db/sudo`, `active` is absent rather than
`false`.

## Git

### git

The repository around the directory you ask from: the nearest `.git` above it, including a linked worktree or a
submodule. Outside a repository there are no `git` fields, and a nested repository shows only its own values. The
compiled-in `git` plugin reads `HEAD` and the refs straight from `.git`, and runs `git` for the rest. Each group of
fields reruns only when the files it depends on change (the working tree, the index or the refs), with polling as a
backstop.

```shell-session
$ comb g git
ahead                  1
behind                 0
branch                 feature/login
branch_default         main
branch_parent          main
commit                 21520f4
commit_summary         Fix login redirect
conflicted             0
detached               false
dirty                  true
last_commit_age_secs   5
last_commit_timestamp  1793740680
lines_added            1
lines_added_default    2
lines_added_parent     2
lines_removed          0
lines_removed_default  0
lines_removed_parent   0
lines_staged_added     1
lines_staged_removed   0
push_ahead             1
push_behind            0
staged                 1
stash                  1
state                  clean
tag                    v0.1.0
unstaged               1
untracked              1
upstream               origin/feature/login
```

**Branch and commit**

| Field | Meaning | Example |
|---|---|---|
| `branch` | Current branch; absent on a detached HEAD | `feature/login` |
| `detached` | Whether HEAD is detached | `false` |
| `commit` | Short hash of HEAD; absent before the first commit | `21520f4` |
| `commit_summary` | First line of HEAD's commit message | `Fix login redirect` |
| `last_commit_timestamp` | When HEAD was committed, in Unix seconds | `1793740680` |
| `last_commit_age_secs` | Seconds since HEAD was committed, worked out from `last_commit_timestamp` as you read it | `3420` |
| `tag` | Nearest tag reachable from HEAD; absent when none | `v0.1.0` |

**Working tree**

| Field | Meaning | Example |
|---|---|---|
| `dirty` | Whether anything is staged, changed, untracked or conflicted | `true` |
| `staged` | Files staged | `1` |
| `unstaged` | Tracked files changed but not staged | `1` |
| `untracked` | Untracked files | `1` |
| `conflicted` | Files with merge conflicts | `0` |
| `lines_added` | Lines added in unstaged changes | `1` |
| `lines_removed` | Lines removed in unstaged changes | `0` |
| `lines_staged_added` | Lines added in staged changes | `1` |
| `lines_staged_removed` | Lines removed in staged changes | `0` |
| `stash` | Stash entries | `1` |

**Remotes**

| Field | Meaning | Example |
|---|---|---|
| `upstream` | The branch's upstream; absent when it has none | `origin/feature/login` |
| `ahead` | Commits ahead of the upstream; absent without one | `1` |
| `behind` | Commits behind the upstream; absent without one | `0` |
| `push_ahead` | Commits ahead of the branch on the push remote (`branch.<name>.pushRemote`, else `remote.pushDefault`); absent when neither is set | `1` |
| `push_behind` | Commits behind it | `0` |

**Operation in progress**

| Field | Meaning | Example |
|---|---|---|
| `state` | `clean`, `merge`, `rebase`, `cherry-pick`, `bisect` or `revert` | `rebase` |
| `state_step` | Current step of a rebase; absent otherwise | `2` |
| `state_total` | Total steps of a rebase; absent otherwise | `5` |

**Lineage**: how far this branch has moved from the branch it came from and from the default branch. These count
committed changes since the merge base, not the working tree, and are absent on a detached HEAD.

| Field | Meaning | Example |
|---|---|---|
| `branch_parent` | The branch this one was created from, per its reflog; else the default branch | `main` |
| `lines_added_parent` | Lines added since leaving `branch_parent` | `2` |
| `lines_removed_parent` | Lines removed since leaving `branch_parent` | `0` |
| `branch_default` | The default branch: `origin/HEAD`'s, else a local `main` or `master`; absent when none is found | `main` |
| `lines_added_default` | Lines added since diverging from `branch_default` | `2` |
| `lines_removed_default` | Lines removed since diverging from `branch_default` | `0` |

When the default branch comes from `origin/HEAD`, the comparison is with `origin/<name>`, so a checkout on the default
branch still counts what it has not pushed.

## Development tools

### python

The Python project around the directory: the nearest directory with a `pyproject.toml` or a virtualenv (`.venv`,
`venv`, `.virtualenv` or `env` holding a `pyvenv.cfg`). Reruns when the virtualenv or `pyproject.toml` changes.

```shell-session
$ comb g.j python
{
  "local_venv_name": ".venv",
  "venv": true,
  "venv_name": ".venv",
  "venv_version": "3.12.4",
  "version": "3.12.4"
}
```

| Field | Meaning | Example |
|---|---|---|
| `venv` | Whether the project has a virtualenv | `true` |
| `local_venv_name` | That virtualenv's directory name | `.venv` |
| `venv_version` | The Python version its `pyvenv.cfg` records | `3.12.4` |
| `version` | Rendered: the version in effect, the first of `$PYENV_VERSION`, `$MISE_PYTHON_VERSION`, `mise.python`, `asdf.python`, `venv_version` | `3.12.4` |
| `venv_name` | Rendered: `local_venv_name`, else the last part of `$VIRTUAL_ENV` | `.venv` |

`version` and `venv_name` are rendered in your shell, so they work outside a project too: with
`VIRTUAL_ENV=/opt/envs/tools` set, `comb g python.venv_name` is `tools`.

### mise

The tools [mise](https://mise.jdx.dev) sets for a directory. At a project, the nearest directory with a `mise.toml` or
`.mise.toml`, it holds only the tools that project's own mise files set, as `mise ls --current` reports them there. Its
overrides: a tool from your global config is not in it. Anywhere else it reads `~`, which holds every tool mise
resolves there, the global ones. The nearest instance answers alone, so a project never shows the global tools beside
its own. Reruns when a mise file there, or mise's global config directory, changes. Needs `mise` on the daemon's
`PATH`; without it the provider is off.

```shell-session
$ comb g.j mise
{
  "cargo:cargo-nextest": "0.9.140",
  "rust": "1.95.0"
}
```

There is one field per tool, named as mise names it: `comb g mise.rust`. A name holding a `.` or `/` (such as
`go:golang.org/x/tools/gopls`) is quoted in brackets: `comb g 'mise["go:golang.org/x/tools/gopls"]'`. For the tools a
prompt should show, read [`tools`](#tools).

### tools

The languages a project uses and the versions of their tools, for a prompt. The project is the nearest directory,
walking up, holding a marker; its markers name its languages, and each language brings its tools:

| Marker | Language | Tools |
|---|---|---|
| `Cargo.toml` | rust | rust, cargo |
| `package.json` | node | node, npm |
| `go.mod` | go | go |
| `pyproject.toml`, `requirements.txt`, `setup.py` | python | python, and uv with a `uv.lock` |
| `Gemfile` | ruby | ruby |
| `composer.json` | php | php |
| `pom.xml`, `build.gradle`, `build.gradle.kts` | java | java |

A nested project is its own: in a repository's `website/` holding a `package.json`, `tools` is node's, not the
repository's rust. A tool's version is mise's at the project, from the project's file or the global one; cargo, which
mise installs with rust, is mise's rust. A tool mise does not list is the one on the daemon's `PATH`, by its version
command (`npm --version`). A tool with no version found is left out. Reruns when a marker, `uv.lock` or a mise file
changes, and every 10 minutes while read.

| Field | Value |
|---|---|
| `langs` | the languages, in the table's order |
| `list` | each tool: `name`, `lang`, `parent` (the language's first tool, for the others), `version`, `minor` (major.minor), `major`, `via` (`mise` or `path`) |
| `versions` | `{tool: version}` |

`tools.prompt` is the one to put in a prompt. It shows one of the styles under `tools.style`, the emoji style with
major.minor versions by default. `comb get tools.style` shows every style at once:

| Template | Each tool as |
|---|---|
| `style.glyph.<part>` | its Nerd Fonts glyph and version |
| `style.colour.<part>` | its glyph in its colour, and version |
| `style.emoji.<part>` | its emoji, else its glyph in colour (node has no emoji), and version |
| `style.label.<part>` | its name and version |

`<part>` is `full` (the whole version), `minor` (major.minor) or `major`. To change the prompt, point it at another
style. A put takes effect on the next read, and without `--global` holds for one directory and below:

```shell-session
$ comb put --global tools:template.prompt '"{{ tools.style.label.full }}"'
```

A tool that comes with its language, such as cargo with rust, is shown only when its version differs from the
language's. Each is absent where there are no tools. The glyphs, colours and emoji are the vars `glyph`, `colour` and
`emoji`, maps by tool name; a var in your config replaces one. The colours are GitHub's language colours lightened to
7:1 contrast on a `#151515` background:

```toml
[client.providers.tools]
var.colour = "{{ {'rust': '#ffb380', 'node': '#9fdf8a'} }}"
```

Every style is one macro, `tools:macro.show`, for a style of your own:
`{% from 'tools:macro.show' import show %}{{ show(comb.tools.list, 'colour', 'minor', glyph, colour, emoji) or none }}`.

```shell-session
$ comb g tools.prompt
🦀 1.95
$ comb g tools.style.label.minor website
node 24.22 npm 11.19
```

### asdf

The tools set in `.tool-versions`: the nearest one above the directory, then the global `~/.tool-versions` (else
`~/.config/asdf/tool-versions`) for any tool the project does not set. One field per tool, holding its first version.
Reads the file itself; asdf does not need to be installed.

```shell-session
$ cat ~/.tool-versions
nodejs 18.0.0
terraform 1.7.0
$ cat ~/tv/.tool-versions
nodejs 20.11.0
ruby 3.3.0
$ comb g.j asdf ~/tv/sub
{
  "nodejs": "20.11.0",
  "ruby": "3.3.0",
  "terraform": "1.7.0"
}
```

### direnv

Whether [direnv](https://direnv.net) allows the nearest `.envrc` above the directory. It reads direnv's own allow and
deny records under `$XDG_DATA_HOME/direnv` (by default `~/.local/share/direnv`), so it needs no process; an edit to the
`.envrc`, or `direnv allow`/`direnv deny`, shows on the next read. Absent outside a directory with an `.envrc`.

| Field | Meaning | Example |
|---|---|---|
| `status` | `allowed`, `blocked` (not yet allowed, or edited since) or `denied` | `blocked` |
| `allowed` | Whether `status` is `allowed` | `false` |

### terraform

The workspace selected in the nearest directory holding `.terraform`. Absent outside one.

| Field | Meaning | Example |
|---|---|---|
| `workspace` | `$TF_WORKSPACE` when your shell sets it, else the workspace in `.terraform/environment`, else `default` | `staging` |

```shell-session
$ comb g terraform.workspace
staging
$ TF_WORKSPACE=prod comb g terraform.workspace
prod
```

## Cloud

The cloud namespaces come in pairs. `aws_profiles` and `gcloud_configs` are cached by the daemon: every profile or
configuration on the machine, whatever your shell has selected. `aws` and `gcloud` are rendered in your shell and pick
the one your environment selects, so two shells with different `AWS_PROFILE` each see their own. Neither needs the
cloud CLI installed.

### aws

The AWS profile and region in effect in your shell.

| Field | Meaning | Example |
|---|---|---|
| `profile` | `$AWS_PROFILE`, else `$AWS_VAULT`, else `$AWS_DEFAULT_PROFILE`, else `default` | `work-prod` |
| `region` | `$AWS_REGION`, else `$AWS_DEFAULT_REGION`, else that profile's region from `aws_profiles` | `eu-west-1` |
| `expiration` | When the session's credentials expire: `$AWS_CREDENTIAL_EXPIRATION`, else `$AWS_SESSION_EXPIRATION` | `2026-11-06T04:00:00Z` |

```shell-session
$ comb g aws
profile  default
region   us-east-1
$ AWS_PROFILE=work-prod comb g aws
profile  work-prod
region   eu-west-1
```

### aws_profiles

Every profile in the AWS config file (`$AWS_CONFIG_FILE`, else `~/.aws/config`), each with its region. A profile with
no region is left out. Watches the file, and polls it every minute.

```shell-session
$ comb g.j aws_profiles
{
  "default": {
    "region": "us-east-1"
  },
  "work-prod": {
    "region": "eu-west-1"
  }
}
```

### gcloud

The Google Cloud project and account in effect in your shell.

| Field | Meaning | Example |
|---|---|---|
| `project` | `$CLOUDSDK_CORE_PROJECT`, else the selected configuration's project | `acme-prod` |
| `account` | The selected configuration's account | `jane@example.com` |

The selected configuration is `$CLOUDSDK_ACTIVE_CONFIG_NAME`, else gcloud's active one.

```shell-session
$ comb g gcloud.project
acme-prod
$ CLOUDSDK_ACTIVE_CONFIG_NAME=default comb g gcloud.project
my-sandbox
```

### gcloud_configs

Every gcloud configuration, from gcloud's config directory (`$CLOUDSDK_CONFIG`, else `~/.config/gcloud`): the active
one's name, and each configuration's project and account keyed by its name. Polled every minute.

```shell-session
$ comb g.j gcloud_configs
{
  "active_config": "work",
  "default": {
    "account": "jane@example.com",
    "project": "my-sandbox"
  },
  "work": {
    "account": "jane@example.com",
    "project": "acme-prod"
  }
}
```

### kubecontext

The kubectl context in effect in your shell, from `kubectl config`. Rendered in your shell, so it honours your
`$KUBECONFIG`. The command's answer is remembered, and run again when `$KUBECONFIG` or a kubeconfig file changes, or
after 10 minutes. Needs `kubectl`.

| Field | Meaning | Example |
|---|---|---|
| `context` | Current context; absent when none is set or `kubectl` is not installed | `dev-cluster` |
| `namespace` | The context's namespace, `default` when it sets none; absent without a context | `web` |

### talos

The [talosctl](https://www.talos.dev) context in effect, from `talosctl config info`. Remembered and rerun as
`kubecontext` is, on `$TALOSCONFIG` or `~/.talos/config`. Needs `talosctl`.

| Field | Meaning | Example |
|---|---|---|
| `context` | Current context; absent when none is set or `talosctl` is not installed | `homelab` |

`kubecontext` and `talos` run a command, so they render only where a template may run commands: `comb get`,
`comb eval`, `comb watch` and `comb status`, and an SDK client created with its `exec` option on (off by default).
Elsewhere the field is the error `exec() is disabled in this context`. Each starts by checking the tool is on your
shell's `PATH` (`'kubectl' | which`) and is absent without it, spawning nothing.

## Containers

### docker

Read in your shell, as `kubecontext` is, because the answer follows `$DOCKER_CONTEXT`, `$DOCKER_HOST` and
`$DOCKER_CONFIG`, which the daemon would see only as they were when it started. Each command runs once per read and is
remembered for 30 seconds. Needs `docker` on your shell's `PATH`; every field is absent without it.

| Field | Meaning | Example |
|---|---|---|
| `context` | `docker context show` | `orbstack` |
| `running` | `docker ps -q` answered (2 second limit); `false` when the engine is down | `true` |
| `count` | Running containers; absent unless `running` | `3` |

A `ps` that hangs past the limit fails the read of `count` and `running`, as any failed command in a template does.

### podman

The same fields, from `podman`, using only its own CLI. `context` is `$CONTAINER_CONNECTION`, else the default of
`podman system connection list` (a Podman machine on macOS has one); absent when there is none, as with the local
socket on Linux. Written from Podman's documentation and not run against a real install.

### containers

A template over both, for themes and plugins written for everyone: `engine` (`podman` or `docker`, the first that is
running, podman first; absent when none is), `running` (absent when neither tool is installed) and `count` (summed;
absent unless one is running).

## Package managers

`brew`, `apt`, `dnf`, `pacman` and `apk` each answer from what the manager already knows: nothing is fetched and
nothing needs `sudo`. Each is off where its tool is not on the daemon's `PATH` (`enabled.template`), and polled hourly.
`brew`, `apt` and `apk` also run again when their package lists or installed-package database change.

| Field | Meaning | Example |
|---|---|---|
| `count` | Outdated packages | `4` |
| `packages` | Their names: `'openssl' in apt.packages` | `["openssl", "curl"]` |
| `checked` | When the package lists were last refreshed, epoch seconds from their files' mtimes; absent when none is found | `1794328194` |

| Provider | Command | Lists dated by |
|---|---|---|
| `brew` | `brew outdated --quiet` with `HOMEBREW_NO_AUTO_UPDATE=1` (brew would otherwise update itself first) | `<brew --repository>/.git/FETCH_HEAD` |
| `apt` | `apt list --upgradable` | newest `Packages`/`Release` file in `/var/lib/apt/lists` |
| `dnf` | `dnf check-update --cacheonly` (exit 100 means updates) | newest `repomd.xml` in dnf's cache |
| `pacman` | `checkupdates --nosync` (pacman-contrib) | `sync/*.db` of its own copy, `${CHECKUPDATES_DB:-${TMPDIR:-/tmp}/checkup-db-$UID}` |
| `apk` | `apk version -l '<'` | `APKINDEX.*` in `/var/cache/apk` or `/etc/apk/cache` |

A manager that fails (no cache yet, a lock) has no fields, not a count of 0.

Refreshing the lists is an action that uses the network, so it is opt-in: `brew`, `dnf` and `pacman` each have an
`update` Source that ships **disabled** (`enabled = false`), and runs daily when you switch it on in your configuration:

```toml
[daemon.providers.brew.sources.refresh]
enabled = true
```

`brew.refresh` runs `brew update --quiet`, `dnf.refresh` `dnf makecache --refresh` (as the daemon's user, so it fills
that user's own cache), `pacman.refresh` a plain `checkupdates`, which syncs the copy `pacman` reads. Each adds one
field to its provider, `at`, when it last ran. `apt` and `apk` have none, because `apt update` and `apk update` need
root. `apt`, `apk`, `dnf` and `pacman` are not run against a real system by the tests, which use stand-in commands
printing recorded output.

## Shell environment

### conda

| Field | Meaning | Example |
|---|---|---|
| `env` | The active conda environment, `$CONDA_DEFAULT_ENV`; absent when none is active | `base` |

### op

| Field | Meaning | Example |
|---|---|---|
| `service_account` | Whether `$OP_SERVICE_ACCOUNT_TOKEN` is set in your shell. Never the token, and nothing about whether it is valid | `false` |

### ssh

Your shell's ssh agent, from `ssh-add -l`, run once per read. Rendered in your shell, so it asks the agent at your
`$SSH_AUTH_SOCK`. The answer is remembered for 30 seconds per socket. The provider is on only where `ssh-add` is on the daemon's `PATH`, and a
read runs a command, so it renders where `kubecontext` does (see above).

| Field | Meaning | Example |
|---|---|---|
| `agent` | Whether an agent answers at `$SSH_AUTH_SOCK` | `true` |
| `keys` | How many keys it holds, `0` when it holds none; absent when there is no agent | `2` |
| `identities` | Each key's comment, the text between its fingerprint and its `(TYPE)`; `[]` when it holds none; absent when there is no agent | `["josh@laptop"]` |
| `forwarded` | Whether `$SSH_AUTH_SOCK` is the socket sshd makes for a forwarded agent, `/tmp/ssh-*/agent.*`. A guess from the path | `false` |

Not shipped: `authorized_keys` and `known_hosts` counts. They say who can log in, not what your prompt should show; write
a [custom provider](../custom-providers/from-a-command.md) if you want them.

### gpg

Your gpg-agent, from `gpg-connect-agent --no-autostart 'keyinfo --list' /bye`, run once per read. It never starts an
agent. Remembered for
30 seconds per `$GNUPGHOME`. On only where `gpg-connect-agent` is on the daemon's `PATH`. Meant as a warning rather than
a display: colour a prompt when `gpg.cached` is `0`, or have a shell plugin say that the next `git commit` will ask for
a passphrase. It names no key.

| Field | Meaning | Example |
|---|---|---|
| `agent` | Whether gpg-agent is running | `true` |
| `keys` | How many keys it holds secrets for; absent when it is not running | `2` |
| `cached` | How many of those have their passphrase cached; absent when it is not running | `0` |

### kerberos

Whether your shell holds a valid Kerberos ticket, from `klist`: on an enrolled machine, a command that wants Kerberos
fails when it does not. One command per read (`klist -s`, then `klist`), remembered for 30 seconds per `$KRB5CCNAME`. On only where `klist` is on the daemon's `PATH`.
It reads MIT Kerberos and macOS's Heimdal alike. There is no expiry field: `ticket` already says whether it has passed.

| Field | Meaning | Example |
|---|---|---|
| `ticket` | Whether the credential cache holds a valid ticket, the exit status of `klist -s` | `true` |
| `principal` | The cache's principal (MIT's `Default principal:`, Heimdal's `Principal:`); absent when there is no cache | `josh@EXAMPLE.COM` |

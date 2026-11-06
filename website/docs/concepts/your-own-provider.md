---
sidebar_position: 6
title: Your own provider
---

# Your own provider

A template is worked out every time it's read, so it has to be quick. Running a command or reading and parsing a
file is too slow to do on every prompt. That work belongs in a **source**, which the daemon runs once and keeps up to
date, as it does for git.

A namespace whose values come from sources is called a **provider**. Here's one that reads the version from a
project's `package.json`:

```json title="~/code/shop/package.json"
{"name": "shop", "version": "1.4.0"}
```

```toml title="~/.config/beachcomber/config.toml"
[daemon.providers.pkg.sources.main]
root = "{{ lup('package.json') | dirname }}"
data = "{{ 'package.json' | read | fromjson }}"
result.name = "{{ data.name }}"
result.version = "{{ data.version }}"
return = "{{ result | watch('package.json') }}"
```

This is a provider called `pkg` with one source, `main`. A source is a table with four keys, and their names are
fixed: anything else is an error. Each is a template, run by the daemon rather than `comb`:

| key | says | required |
|---|---|---|
| `root` | which directory the values belong to: here, the nearest one with a `package.json`, going up. Without it, the values are global | no |
| `data` | the work: here, read `package.json` and parse it | yes |
| `result.<name>` | a value, picked out of what `data` found. You choose the names: `result.version` is read as `pkg.version` | yes |
| `return` | when to run the source again: here, when `package.json` changes. Without it, every minute while it's being read. [Keeping a source current](./keeping-a-source-current.md) covers it | no |

`data` and `result` are separate so that the work runs once and any number of values come out of it. git's `status`
source runs `git status` once and gives `staged`, `unstaged`, `untracked` and more from it. It also lets a later file
change one `result` line, or just the `data`, without repeating the rest. `exec` and `url` work only in `data`.

`root` is how a provider takes part in [the cascade](./cascade.md), the same way git's values belong to the
repository.

```shell-session
$ cd ~/code/shop/web
$ comb get pkg
name=shop
version=1.4.0
```

Edit the version in `package.json` and read it again: you get the new one. The daemon notices when a file a source
read has changed, and runs it again.

## Projects inside projects: `nearest`

A repository can hold packages with `package.json` files of their own. Each one is its own root, and the cascade
works across them: a value the inner package doesn't have falls up to the outer one.

```json title="~/code/mono/package.json"
{"name": "mono", "version": "2.0.0"}
```

```json title="~/code/mono/packages/ui/package.json"
{"name": "ui"}
```

```shell-session
$ cd ~/code/mono/packages/ui
$ comb get pkg
name=ui
version=2.0.0
```

`ui` has no version, so `pkg.version` came from `mono`. Sometimes that's what you want. When it isn't, add `nearest`
to the `return` line: the nearest root answers alone, and nothing falls up from the ones above it.

```toml
return = "{{ result | watch('package.json') | nearest }}"
```

```shell-session
$ comb get pkg
name=ui
```

git's sources all use `nearest`, so a repository inside another one never shows the outer repository's branch.

## Reading other values

A source's `data` line reads other values by name, as a template does: `data = "{{ git.branch }}"`. A source runs in
the daemon, which only stores values from sources and puts, so it sees those and not templates. There, `git.branch`
and `comb.git.branch` mean the same thing. `comb check config` warns if a source reads a value only a template
gives, such as `aws.profile`, since it would always be empty.

## Several sources in one provider

A provider can have more than one source, and their values merge into one namespace. git has several, so that
switching branch doesn't recount every changed line: each source is run again only when its own inputs change.

## What a source can do

`data` can read files, run commands with `exec`, and fetch from web APIs with `url`. The
[Custom Providers](../custom-providers/overview.md) guides have a recipe for each, plus how to say how often a source
runs again.

**Next:** [A value's life](./a-values-life.md)

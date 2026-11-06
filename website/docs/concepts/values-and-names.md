---
sidebar_position: 1
title: One tree of values
---

# One tree of values

Everything comb knows is one big tree of values, shaped like a JSON object. At the top are **namespaces**, such as
`git` and `battery`; inside each are its values, and a value can itself hold more values.

```json
{
  "git":     { "branch": "main", "dirty": false, "staged": 0, … },
  "battery": { "percent": 80, "charging": false, … },
  "team":    { "name": "checkout", "links": { "chat": "#ops", "board": "https://example.com/board" } }
}
```

You read it by **path**: the names along the way, joined with dots. Read at any depth, and you get everything
below that point:

```shell-session
$ comb get git.branch
main
$ comb get team.links.chat
#ops
$ comb get battery
charging=false
percent=80
status=unknown
```

A single value prints as itself. Anything with values inside prints one `path=value` line each; add `-f json` to get
it as JSON instead:

```shell-session
$ comb get team -f json
{
  "links": {
    "board": "https://example.com/board",
    "chat": "#ops"
  },
  "name": "checkout"
}
```

## A missing value is empty

Ask for a path that isn't in the tree and you get nothing, not an error:

```shell-session
$ cd ~
$ comb get git.branch
$
```

Outside a repository there is no branch. A prompt that asks for it shows nothing rather than breaking, and a
template can say what to show instead.

## Where you are matters

The tree you read depends on the directory you're in. `battery.percent` is the same everywhere, but `git.branch` is
the branch of the repository you're standing in. Some values are **global**, the same everywhere; others belong to a
**directory** and everything under it.

How comb decides which directory's value you get is the next page.

[Built-in Providers](../providers/built-in-providers.md) lists every namespace that ships.

**Next:** [The cascade](./cascade.md)

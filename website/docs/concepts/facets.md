---
sidebar_position: 10
title: Where did this value come from?
---

# Where did this value come from?

A value can come from a source, a put or a template, and the cascade can find it in any directory above you. When a
value surprises you, you can look at each part on its own.

Add a colon and one of these to a namespace's name:

| ask for | to see |
|---|---|
| `team:put` | only the values that were put |
| `git:source` | only the values sources made |
| `prompt:template` | the templates themselves, not worked out |
| `git:cache` | what the daemon knows about each value |

These views are called **facets**. Each one has the same shape as the namespace, so you can read a whole facet or
one value in it:

```shell-session
$ comb get team:put
name=checkout
$ comb get prompt:template
where={{ git.branch ~ ' · ' if git.branch }}{{ team.name }}
$ comb get git:source.branch
main
```

## What the daemon knows

`:cache` answers "how old is this, and what keeps it fresh?":

```shell-session
$ comb get git:cache.branch
age_ms=26326
source=source(head)
watch.0=.git/HEAD
…
```

`git.branch` was made 26 seconds ago by git's `head` source, and is made again when `.git/HEAD` changes.

## Every layer at once

`comb status --tree` shows each value with every layer behind it: which put, source or template could answer, in
the order the cascade tries them.

That's the whole of comb: one tree of named values, each found by going up from where you are, each made by a
source, a put or a template.

## Where next

- [Custom Providers](../custom-providers/overview.md): recipes for your own values.
- [Built-in Providers](../providers/built-in-providers.md): everything that ships.
- [Template library](../reference/template-library.md): every filter and function.
- [Command reference](../reference/cli-commands.md): every command and flag.

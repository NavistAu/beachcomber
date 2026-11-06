---
sidebar_position: 3
title: Where values come from
---

# Where values come from

Every value in comb is made in one of three ways. From the outside they look the same: you ask for a name and get a
value. What differs is who makes it and when.

| | made by | when | example |
|---|---|---|---|
| a **source** | the comb daemon | once, then kept up to date | `git.branch` |
| a **put** | you, or a script | when you run `comb put` | `team.name` |
| a **template** | `comb` itself, as you read | every time it's read | `prompt.where` |

## Sources: worked out once, kept fresh

The first `comb` command you run starts a small background program, the **daemon**. It does the slow work, like
asking git or reading the battery, and keeps the answers in memory. A **source** is one piece of that work.

The daemon only does work someone needs. The first read of a value runs its source; after that, the daemon keeps
the answer up to date in one of two ways:

- **Watched**: it runs the source again when the files behind it change. `git.branch` comes from `.git/HEAD`, so a
  `git switch` updates it at once.
- **Polled**: things with no file to watch, like the battery, are checked again on a timer.

A value nobody reads for a while is checked less and less often, then dropped. The next read works it out again.

`comb status` shows every value the daemon holds, how old it is, and when it will next be checked.

## Puts: values you store

`comb put` stores a value, as you saw in [the cascade](./cascade.md). It's how scripts and hooks publish things for
your other tools to read: a deploy status, a timer, the result of a slow check. `--ttl 5m` makes one expire on its
own.

A put lives in the daemon's memory, so if the daemon restarts, it's gone. A script that publishes a value should
put it each time it runs.

## Templates: worked out when read

A template is a value made from other values, like "the branch, then the team name". `comb` works it out at the
moment you read it, in your directory and with your shell's environment, and doesn't keep it. That makes templates
the right tool for anything that depends on your current shell, like `$AWS_PROFILE`.

The next page shows how to write one.

**Next:** [Templates](./templates.md)

---
sidebar_position: 2
title: The cascade
---

# The cascade

When you ask for a value, comb starts in your current directory and looks for an answer there. If there isn't one,
it goes up a directory and looks again, and keeps going up until it finds one. Above everything is the **global**
value, the answer for when no directory has one.

**The nearest answer wins.** This is the cascade, and every read in comb works this way.

## See it with your own values

`comb put` stores a value of your own. Store one globally, and another for one project:

```shell-session
$ comb put team.name '"platform"' --global
$ comb put team.name '"checkout"' --path ~/code/shop
```

(The value is JSON, which is why the text has quotes inside the quotes.)

Now the answer depends on where you ask:

```shell-session
$ cd ~/code/shop/web
$ comb get team.name
checkout
$ cd ~/notes
$ comb get team.name
platform
```

In `~/code/shop/web`, comb finds nothing in `web`, goes up to `~/code/shop`, and finds `checkout`. In `~/notes`, it
goes all the way up and finds the global value.

Remove the project's value by putting `null`, and the global one shows through again:

```shell-session
$ comb put team.name null --path ~/code/shop
$ cd ~/code/shop/web
$ comb get team.name
platform
```

## Globals are defaults

Because the nearest answer wins, a global value never overrides a directory's own. It only fills in where nothing
nearer has an answer. Set things globally as sensible defaults, and per project where a project differs.

## Each value on its own

The cascade works one value at a time. If `~/code/shop` sets `team.name` and the global level sets `team.channel`,
a read of `team` in the shop gets both: the shop's name and the global channel.

```shell-session
$ comb put team.channel '"#ops"' --global
$ comb put team.name '"checkout"' --path ~/code/shop
$ cd ~/code/shop
$ comb get team
channel=#ops
name=checkout
```

## Built-in values cascade too

`git` follows the same rule. Its values belong to the repository's top directory, so from anywhere inside the
repository, going up finds them. That's why `git.branch` gives the same answer in `~/code/shop/web/src` as in
`~/code/shop`.

**Next:** [Where values come from](./where-values-come-from.md)

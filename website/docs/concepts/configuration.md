---
sidebar_position: 5
title: Configuration
---

# Configuration

comb works with no configuration at all. You add some when you want values of your own that are more than a
`comb put`: a template with a name, or a new source.

## Where it lives

Your configuration is one file, `~/.config/beachcomber/config.toml`. Create it when you first need it. comb notices
when you save it, so there's nothing to restart.

As it grows, you can split it into files in `~/.config/beachcomber/conf.d/`. They're read in name order after
`config.toml`, so `50-work.toml` can change something `config.toml` set.

## Name a template

The simplest thing to configure is a template with a name. This makes a namespace `prompt` with one value, `where`:

```toml title="~/.config/beachcomber/config.toml"
[client.providers.prompt]
template.where = "{{ git.branch ~ ' · ' if git.branch }}{{ team.name }}"
```

```shell-session
$ cd ~/code/shop
$ comb get prompt.where
main · checkout
```

Your prompt can now read `prompt.where`, and you change what it shows here, in one place, for every tool.

A long template can span several lines inside TOML's `'''…'''`; line breaks inside `{{ }}` don't reach the output.

## Two kinds of table

That table starts with `client`. A configuration file has two kinds, matching the two places work happens:

- **`[client…]`** holds templates. They're worked out by `comb` as you read, in your shell.
- **`[daemon…]`** holds sources and settings. They're run by the daemon, once, for everyone.

The next page adds a `[daemon…]` table: a source of your own.

## When something is wrong

If you save a file with a mistake in it, comb keeps running with the configuration it had. `comb check config`
says what's wrong and where:

```shell-session
$ comb check config
  [FAIL] local    config(~/.config/beachcomber/config.toml): [client.providers.prompt] template.where:
                  syntax error: unexpected end of input, expected end of variable block
```

## The built-in configuration

Everything comb knows out of the box, git and battery included, is written as ordinary configuration, in files comb
keeps in `~/.config/beachcomber/defaults/`. Read them to see how any built-in value is made. Don't edit them: comb
rewrites them when it's upgraded. To change one, set the same thing in your own file, which is read after them and
wins. [Change a built-in provider](../custom-providers/change-a-shipped-provider.md) shows how.

Every setting is listed in the [configuration reference](../reference/configuration-reference.md).

**Next:** [Your own provider](./your-own-provider.md)

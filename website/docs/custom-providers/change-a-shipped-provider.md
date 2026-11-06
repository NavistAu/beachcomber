---
sidebar_position: 9
title: Change a built-in provider
---

# Retune or switch off a shipped provider

Every provider comb ships is a file the daemon writes into `~/.config/beachcomber/defaults/` (`git.toml`,
`battery.toml`, ...). Those files are the lowest layer of configuration, and the daemon rewrites them when comb is
upgraded, so do not edit them. Set the same key in `config.toml` or a `conf.d/` file instead: your line replaces that one
line, and the rest of the shipped file still applies.

Read the shipped file first to see the line you are replacing:

```sh
cat ~/.config/beachcomber/defaults/git.toml
```

## Change how often a Source runs

A Source's schedule is its `return` line. Copy the shipped line and change what you need. To poll git's `diff` Source
every 15 seconds instead of 30, eight times while read:

```toml
# ~/.config/beachcomber/conf.d/git.toml
[daemon.providers.git.sources.diff]
return = '''{{ result
  | watch('.', '!.git')
  | poll('15s', 8)
  | failback('60s', 3)
  | budget('100ms')
  | nearest }}'''
```

Your `return` replaces the shipped one **whole**: a decorator you leave out is not kept from it. Leave out `| nearest`
and git's `diff` fields start cascading up to `~`; leave out `watch` and edits stop triggering a refresh.

Check the result in the `:cache` facet, which shows the schedule each field is on:

```sh
cd ~/repo && comb get git:cache.lines_added -f json
```

```json
{
  "age_ms": 38,
  "decay": 4,
  "executing": false,
  "hash": "2f75d481cc55d4c3",
  "poll": {
    "count": 8,
    "interval": "15s"
  },
  "source": "source(diff)",
  "watch": [
    ".",
    "!.git"
  ]
}
```

`comb check config --origins` shows which file each line comes from, and the line it overrode:

```sh
comb check config --origins daemon.providers.git.sources.diff.return
```

```
  KEY                                       SOURCE                                           VALUE
  daemon.providers.git.sources.diff.return  config(~/.config/beachcomber/conf.d/git.toml)    {{ result | watch('.', '!.git') | poll('15s', 8) | failback('60s', 3) | budget('100ms') | nearest }}
  └─ shadowed                               config(~/.config/beachcomber/defaults/git.toml)  {{ result | watch('.', '!.git') | poll('30s', 4) | failback('60s', 3) | budget('100ms') | nearest }}
```

Any other line of a shipped Source (`root`, `data`, a `result.<field>`) is replaced the same way.

## Change a shipped template

Template-only providers (`aws`, `conda`, `gcloud`, `kubecontext`, `op`, `python`, `talos`, `terraform`) are templates
under `[client.providers.<p>]`. Override one leaf and keep the others:

```toml
[client.providers.aws]
template.profile = "{{ env.AWS_PROFILE or 'none-set' }}"
```

```sh
comb get aws.profile                       # none-set
AWS_PROFILE=work comb get aws.profile      # work
```

`aws.region` and `aws.expiration` still come from `defaults/aws.toml`.

## Keep entries longer, or shorter

How many backoff steps an unread entry takes before it is evicted is the provider's **decay**, 4 by default. Set it per
provider, from 1 to 16:

```toml
[daemon.providers.kv.cache._]
decay = 2
```

or at runtime with `comb put 'kv:cache._.decay' 2`.

## Switch a provider off

```toml
[daemon.providers.battery]
enabled = false
```

None of the provider's Sources runs, is polled or is watched, and a read is a miss:

```sh
comb get battery -f json
```

```
null
```

```sh
comb refresh battery
```

```
error: battery: nothing to refresh: battery is disabled
```

`comb check providers` lists it as `disabled`. A template-only provider switches off the same way. A later file can set
`enabled = true` to turn it back on.

## Add to a provider's answer

A provider's own `return` line shapes everything it answers. `| super` hands the value on to the shipped default line,
so this adds a field and keeps the rest:

```toml
[daemon.providers.kv]
return = "{{ result | merge({'label': 'kv'}) | super }}"
```

```sh
comb get kv -f json
```

```json
{
  "label": "kv",
  "name": "demo",
  "version": "1.2.3"
}
```

Changing a provider's `result` or `return` makes a field read run every Source of the provider, since the line now
decides what exists; keep such lines to providers with cheap Sources. The rules are under
[provider lines](../reference/configuration-reference.md#provider-lines).

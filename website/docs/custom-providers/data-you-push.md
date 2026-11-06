---
sidebar_position: 7
title: Data you push
---

# Data you push with `comb put`

When a program already knows a value (a deploy script, a build, a long-running job), have it write the value into comb
with `comb put`. No configuration is needed: the first `put` to a name creates the provider. Readers get the value the
same way as any other, `comb get <provider>.<field>`.

## Write and read

```sh
cd ~/app
comb put deploy '{"env":"staging","build":412}'
comb get deploy
```

```
build=412
env=staging
```

The data is JSON. At the provider's root it must be an object; below it, any JSON value:

```sh
comb put deploy '[1,2]'
```

```
error: deploy: put data at the root must be an object
```

## Where a value applies

A `put` is stored at a directory: the current one, or the one `--path`/a trailing path argument names. It answers for
that directory and every directory below it. `--global` writes the global entry, `~`, which is the fallback for every
directory:

```sh
comb put --global deploy.env '"prod"'

cd ~            && comb get deploy.env     # prod
cd ~/app/src    && comb get deploy.env     # staging: ~/app's put is nearer
```

The nearer directory wins, field by field. [The cascade](../concepts/cascade.md) explains how values fall up the
directory chain.

## Change and remove

A `put` replaces the node it addresses, so `comb put deploy.env '"qa"'` changes one field and leaves `build`. `null`
removes a node:

```sh
cd ~/app
comb put deploy.build null
comb get deploy -f json
```

```json
{
  "env": "staging"
}
```

## Values that expire

`--ttl` gives a value a lifetime: it is valid for that long, kept for one more step, then removed. Each `put` renews it,
which suits a heartbeat:

```sh
comb put --global --ttl 3s job '{"state":"running"}'
comb get job.state        # running
sleep 15
comb get job.state        # (nothing: the value has expired)
```

The ttl is whole seconds, at least `1s`.

## Over a Source's value

A `put` may also write to a provider that has Sources. It is a layer over what the Source returned at that directory:
the put wins there, and removing it shows the Source's value again.

```sh
comb get kv.version                          # 1.2.3, from the Source
comb put --global kv.version '"9.9.9"'
comb get kv.version                          # 9.9.9
comb put --global kv.version null
comb get kv.version                          # 1.2.3
```

## Keeping a value across a restart

`--persist` asks the daemon to keep a put when it restarts. It writes the put to
`$XDG_CACHE_HOME/beachcomber/puts.json` (`~/.cache/beachcomber/puts.json` by default) and puts it again when it starts.
A put with a ttl comes back with the time it has left, and not at all once that has run out:

```sh
comb put --global --persist editor '{"theme":"dark"}'
comb kill && comb get editor.theme      # dark: the daemon that started put it again
```

The latest put to an address decides: a put there without `--persist`, `null`, or the ttl running out drops the kept
copy. `comb get editor:cache.theme.persist` is `true` while it is kept.

This is best effort, not a guarantee. The file is in the cache directory, which the system or you may clear at any time,
and a value can be lost. Use it for convenience, not for data you cannot recreate.

## Lifetime

Puts live in the daemon's memory. They last until they expire or are removed, or the daemon stops; one made with
`--persist` is put again when the daemon starts. Write them from the program that owns the value each time it changes,
not once at login.

From code, the SDKs have the same operation (`put`); see [Libraries and SDKs](../ecosystem/overview.md). Every flag is
under `comb p` in [CLI Commands](../reference/cli-commands.md).

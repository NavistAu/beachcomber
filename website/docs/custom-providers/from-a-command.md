---
sidebar_position: 4
title: From a command
---

# A provider from a command or script

Wrap a command or a script of your own in a Source. The daemon runs it, caches its fields, and runs it again on the
schedule you give.

## A script that prints JSON

Write the script:

```sh
#!/bin/sh
# ~/.config/beachcomber/providers/docker-context.sh
# Prints the active Docker context as JSON.
context=${DOCKER_CONTEXT:-default}
printf '{"context":"%s","remote":%s}\n' "$context" false
```

```sh
chmod +x ~/.config/beachcomber/providers/docker-context.sh
```

Declare a Source for it:

```toml
# ~/.config/beachcomber/conf.d/docker.toml
[daemon.providers.docker_ctx.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/docker-context.sh') | fromjson }}"
result = "{{ data }}"
return = "{{ result | poll('30s') }}"
```

- `data` runs the script and parses its stdout with `fromjson`.
- `result = "{{ data }}"` makes the object's keys the provider's fields.
- `return` re-runs it every 30 seconds while something reads it.

Read it:

```sh
comb get docker_ctx
```

```
context=default
remote=false
```

```sh
comb get docker_ctx.context
```

```
default
```

The first read runs the script and waits for it. Every read after that is answered from the cache.

## Shaping the output

The filter after `exec()` decides how stdout is read, and the `result` lines pick the fields.

**JSON.** `| fromjson`, then `result = "{{ data }}"` for every key, or `result.<field>` lines for some of them.

**`key=value` lines.** `| fromini` reads them as INI. Keys before any `[section]` land under the empty section `''`:

```toml
[daemon.providers.kv.sources.main]
data = "{{ exec('printf \"name=demo\\nversion=1.2.3\\n\"') | fromini }}"
result.name = "{{ data[''].name }}"
result.version = "{{ data[''].version }}"
```

```sh
comb get kv -f json
```

```json
{
  "name": "demo",
  "version": "1.2.3"
}
```

INI values are strings. Convert one with a filter: `"{{ data[''].count | int }}"`.

**One value.** `exec()` already strips one trailing newline; `| trim` strips the rest of the whitespace. Name the field
with a `result.<field>` line:

```toml
[daemon.providers.greeting.sources.main]
data = "{{ exec(['echo', 'hello=world', 'lang=en']) }}"
result.text = "{{ data }}"
```

```sh
comb get greeting.text
```

```
hello=world lang=en
```

## Running the command

- A **string** runs under `sh -c`, so pipes and `~` work: `exec('ls | wc -l')`.
- A **list** is the argument vector with no shell: `exec(['echo', 'hello=world'])`. Each element is one argument and
  nothing in it is expanded.
- The command's environment is the daemon's, including the [env file](./from-an-http-api.md#keep-secrets-in-the-env-file).
  Write a variable into the line (`exec('tool --region ' ~ env.REGION)`) rather than as `$REGION` in the string, so the
  value the command gets is visible in the line.
- `timeout=` cuts the command off (default `'10s'`): `exec('slow-tool', timeout='30s')`.
- A Source with a `root` runs its command in that directory (below).

## Choosing a schedule

`return` passes `result` through **decorators** that set when the Source runs again. Without a `return` line, a Source
takes the shipped default, `poll('60s', 12) | failback('1s', 3)`.

| Data changes | Write |
|---|---|
| on its own, whatever the files do | `return = "{{ result \| poll('10s') }}"` |
| only when some files change | `return = "{{ result \| watch('.tool-versions', 'package.json') }}"` |
| with files, but an event may be missed | `return = "{{ result \| watch('Cargo.toml') \| poll('120s') }}"` |
| rarely, and the value may simply expire | `return = "{{ result \| ttl('1h') }}"` |

A relative `watch` path is relative to the Source's `root`; one starting with `/`, `~` or `$` is absolute. `'.'` watches
the whole root, and `'!.git'` after it leaves a directory out. Every decorator and its arguments are in the
[reference](../reference/configuration-reference.md#return-decorators).

Polling only lasts while something reads the value. After `poll`'s count of runs (12 by default) with no reader, the
Source backs off and is eventually evicted; the next read runs it again.

## One instance per project

A `root` line gives the Source one instance per directory it finds. `lup(name)` finds the nearest `name` at or above
the directory being read:

```toml
[daemon.providers.todo.sources.main]
root = "{{ lup('.git') | dirname }}"
data = '''{{ exec('git ls-files --others --exclude-standard | wc -l', timeout='2s')
  | trim
  | int }}'''
result.untracked = "{{ data }}"
return = "{{ result | watch('.', '!.git') | poll('5m') | nearest }}"
```

The command runs in the repository's root, so `git ls-files` lists that repository. From anywhere inside it:

```sh
cd ~/repo/src && comb get todo.untracked
```

```
3
```

Creating a file in the repository fires the watch, and the next read shows `4`.

`| nearest` makes the nearest instance answer alone: outside a repository there is no instance, and the read is a miss
(`comb get` prints nothing and exits 0; with `--strict` it exits 1). Without `nearest`, a field the nearest instance lacks
falls back to the instance above it, up to `~`. [The cascade](../concepts/cascade.md) explains both.

## When the command fails

A non-zero exit, a timeout or output the filter cannot parse fails the run. The last good value stays in the cache and
readers keep getting it. The failure, with the tail of the command's stderr, goes to the daemon log
(`$XDG_STATE_HOME/beachcomber/daemon.log`):

```
WARN beachcomber::run: Source 'flaky.main' failed: flaky.main: data: `sh -c 'cat ~/flag.json'`: exit status: 1: cat: /home/me/flag.json: No such file or directory
```

After repeated failures the Source backs off. `failback(interval, count)` on `return` sets how: after `count` failures in
a row, retries are spaced from `interval`, doubling. A command that succeeds has its stderr dropped.

## Keep it fast

Each run of `exec()` starts a process, which costs milliseconds. For a value that lives in a file, read the file instead
([From files](./from-files.md)); for an HTTP API, use `url()` ([From an HTTP API](./from-an-http-api.md)). Poll a command
no faster than its value changes.

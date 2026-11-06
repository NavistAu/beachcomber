---
sidebar_position: 9
title: Caching a template with cache()
---

# Caching a template with `cache()`

A template is worked out every time it's read. When it runs a command, that command runs on every read too, and a
prompt reads a lot. `cache()` gives a template a kept answer, so the command runs once and later reads reuse it.

To see it working, use a command whose answer changes every second: `date`.

## Without `cache()`

```toml title="~/.config/beachcomber/config.toml"
[client.providers.time]
template.every_read = "{{ 'date +%T' | exec() | trim }}"
```

Each example on this page is one more value in the `time` namespace, written as `template.<name>`. The names say
what each example shows: `time.once` is worked out once, `time.for_5s` is kept for 5 seconds.

```shell-session
$ comb get time.every_read
05:26:39
$ comb get time.every_read
05:26:41
```

`date` ran on both reads.

## With `cache()`

Put `cache()` before the work it should save:

```toml
template.once = "{{ 'date +%T' | cache() | exec() | trim }}"
```

```shell-session
$ comb get time.once
05:26:41
$ comb get time.once
05:26:41
```

The first read ran `date` and kept the answer. The second found the kept answer at `cache()` and stopped there:
`exec()` never ran.

Everything before `cache()` decides whether the kept answer can be used. Everything after it is the work that's
skipped when it can.

### How `cache()` knows the answer still fits

Each time it's reached, `cache()` builds a **key** from what it's given, and the kept answer carries the key it was
made with. Same key: the kept answer is used. Different key: the work runs, and its answer replaces the kept one.

The key is made from:

- **`cache()`'s arguments**, if you give it any: `cache(env.STAGE, env.REGION)`. Any value works: an environment
  variable, another comb value, a string.
- **Otherwise, the value flowing into it**, which is usually the command. That matters when the command is built
  from other comb values: here the commit is part of the command, so a new commit makes a new key and the subject is
  looked up again, while every other read reuses it.

  ```toml
  template.subject = "{{ ('git log -1 --format=%s ' ~ git.commit) | cache() | exec() | trim }}"
  ```

  ```shell-session
  $ comb get last.subject
  init
  $ git commit --allow-empty -m 'fix the login button'
  $ comb get last.subject
  fix the login button
  ```
- **The files named by `watch()`** before it, as they are now: their size, modification time and identity. A file
  that changes, appears or disappears changes the key.

Nothing else goes in. Not your directory: there's one kept answer per value, and a read from any directory below the
one it was made in uses it too. And not your environment, unless you name a variable in `cache()`. comb can't see
which variables a command reads, so a command that depends on `$KUBECONFIG` needs `cache(env.KUBECONFIG)`.

There's only one kept answer per value, so a key that changes back and forth runs the work each time it changes:

```toml
template.per_place = "{{ 'date +%T' | cache(env.STAGE, env.REGION) | exec() | trim }}"
```

```shell-session
$ STAGE=dev REGION=au comb get time.per_place
06:20:44
$ STAGE=dev REGION=au comb get time.per_place
06:20:44
$ STAGE=dev REGION=us comb get time.per_place
06:20:46
$ STAGE=dev REGION=au comb get time.per_place
06:20:47
```

`comb get time:cache.once` shows the key, as `hash`.

## Saying when the answer is out of date

A kept answer with nothing else said lasts a long time, a few hours. Usually you know better, and you say so before
`cache()`.

**After a while: `ttl()`.** The kept answer is used for this long, then the next read works it out again:

```toml
template.for_5s = "{{ 'date +%T' | ttl('5s') | cache() | exec() | trim }}"
```

```shell-session
$ comb get time.for_5s
05:26:44
$ comb get time.for_5s          # 2 seconds later
05:26:44
$ comb get time.for_5s          # 6 seconds later
05:26:50
```

There's no `poll()` here, as there is for a source. Only the daemon runs things on a timer, and a template is worked
out by `comb` in your shell, with your environment and directory, so it can only be worked out again when something
reads it.

**When a file changes: `watch()`.** The kept answer is dropped as soon as the file changes:

```toml
template.until_touched = "{{ 'date +%T' | watch('~/flag') | cache() | exec() | trim }}"
```

```shell-session
$ comb get time.until_touched
05:26:50
$ touch ~/flag
$ comb get time.until_touched
05:26:51
```

Use both when you can: `watch()` catches the change you expect, and `ttl()` catches the ones you don't.

**When a variable changes: `cache(env.X)`.** The variable goes into the key, so a different value works the answer
out again:

```toml
template.per_stage = "{{ 'date +%T' | cache(env.STAGE) | exec() | trim }}"
```

```shell-session
$ STAGE=dev comb get time.per_stage
05:26:51
$ STAGE=prod comb get time.per_stage
05:26:53
```


## A real one

This is the shipped `kubecontext.context`, all three together:

```toml
template.context = '''{{ 'kubectl config current-context'
  | watch(env.KUBECONFIG or '~/.kube/config')
  | ttl('10m')
  | cache(env.KUBECONFIG)
  | exec() }}'''
```

`kubectl` runs when the kube config changes, when `$KUBECONFIG` points somewhere else, or at most every ten minutes.
Every other prompt gets the kept answer.

A template this long reads better over several lines. A TOML string in `'''` can span lines, and line breaks inside
`{{ }}` don't reach the output, so this is the same template as the one-line version.

## Seeing the kept answer

`:cache` shows a template's kept answer and how old it is:

```shell-session
$ comb get time:cache.once
age_ms=11320
source=memo
…
```

`comb get --force` ignores the kept answer and works it out again.

The kept answer lives in the daemon, so every shell in the directory shares it. `comb eval` and `-f` never keep
anything: in them, `cache()` does nothing.

**Next:** [Where did this value come from?](./facets.md)

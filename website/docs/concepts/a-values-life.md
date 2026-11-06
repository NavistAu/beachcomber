---
sidebar_position: 7
title: A value's life
---

# A value's life

comb keeps answers so it doesn't work them out again. A kept answer can go out of date, so every value has a life:
how it starts, how it's kept current, and when it's let go. This page follows two built-in values through theirs:
`git.branch`, which changes when a file changes, and `battery.percent`, which changes with time.

## 1. Born on the first read

Nothing is worked out until someone asks. The first `comb get git.branch` in a repository runs git's source, keeps
the answer, and returns it. Every read after that gets the kept answer.

## 2. Kept current while it's being read

While a value is being read, comb keeps it current in one of two ways.

**When a file changes.** `git.branch` comes from `.git/HEAD`. comb watches that file, and runs the source again when
it changes. A `git switch` updates the branch at once, with no waiting.

**On a timer.** The battery has no file to watch, so comb runs its source every 30 seconds instead.

## 3. Let go when nobody reads it

A value only stays current while it's being read. When the reads stop, comb slows down: the battery's 30-second
check becomes a minute, then two, then four, then eight. After four steps the value is dropped. The next read starts
its life again at step 1.

This is why comb can serve every repository you've ever opened without the work piling up: only what's being read is
kept current.

## Watching it happen

`comb status` shows where each value is in its life (some columns left out here):

```shell-session
$ comb status
  PROVIDER  FIELD    VALUE  AGE    POLL  TTL
  battery   percent  80     12s×1   18s  ★  30s×04 (2m)
  git       branch   main   41s       -  ★               ∙
```

- **AGE** is how long ago the answer was worked out. `×1` counts the checks since anyone last read it.
- **POLL** is the time until the next check on a timer. git's branch has none: it waits for its file.
- **TTL** is the stage. `★` means it's being read and kept current. `30s×04` is the schedule: every 30 seconds, and
  after 4 checks with no read it starts slowing down. A number in place of `★` counts the slowing-down steps left.
  `∙` means it's watching a file.

## Setting it yourself

A source's `return` line sets how it's kept current: `watch()` for a file, `poll()` for a timer, `ttl()` to expire.
A template that runs a command can keep its answer too, with `cache()`. The next two pages build each one up with an
example.

**Next:** [Keeping a source current](./keeping-a-source-current.md)

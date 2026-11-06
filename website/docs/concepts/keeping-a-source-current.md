---
sidebar_position: 8
title: Keeping a source current with watch(), poll() and ttl()
---

# Keeping a source current with `watch()`, `poll()` and `ttl()`

A source's `return` line decides when the daemon runs it again. It passes the source's `result` through one or more of
`watch()`, `poll()` and `ttl()`.

To see each one working, use a source whose answer changes every second, the time from `date`:

```toml title="~/.config/beachcomber/config.toml"
[daemon.providers.clock.sources.by_default]
data = "{{ exec('date +%T') | trim }}"
result.by_default = "{{ data }}"
```

Each example below is another source in this `clock` provider, the same as this one but for its `return` line. The
source and its value are named after what the example shows, so `clock.every_2s` is the one polled every 2 seconds.

## With no `return` line

```shell-session
$ comb get clock.by_default
05:30:10
$ comb get clock.by_default       # 3 seconds later
05:30:10
```

The daemon ran it once and kept the answer. With no `return` line, a source runs again every 60 seconds while it's
being read. That suits most things, but not a clock.

## On a timer: `poll()`

`poll(every, count)` runs the source on a timer:

```toml
return = "{{ result | poll('2s', 3) }}"
```

```shell-session
$ comb get clock.every_2s
05:30:13
$ comb get clock.every_2s      # 3 seconds later
05:30:15
```

The daemon ran it again on its own, every 2 seconds, with nobody waiting on it.

The second number, `3`, is how many runs in a row can go unread before the daemon starts slowing down. After 3
unread runs, the 2 seconds becomes 4, then 8, and so on, and after four slow-downs the value is dropped. A read at any
point puts it back on its 2-second timer. `comb status` shows this:

```shell-session
$ comb status --filter provider=clock
  FIELD    VALUE     AGE   POLL  TTL
  every_2s   05:30:30  0s×0    3s  3  4s×03 (12s)
```

`3  4s×03` reads: three slow-downs left, now running every 4 seconds.

## When a file changes: `watch()`

`watch(path)` runs the source when a file or directory changes, and not otherwise:

```toml
return = "{{ result | watch('~/flag') }}"
```

```shell-session
$ comb get clock.on_touch
05:30:16
$ touch ~/flag
$ comb get clock.on_touch
05:30:18
```

A relative path is relative to the source's `root`, so `watch('.git/HEAD')` watches the repository the source belongs
to. That's how `git.branch` changes the moment you switch branch.

## Expire instead: `ttl()`

`ttl(time)` doesn't run the source on its own. The answer is kept for that long, and after that the next read runs
it:

```toml
return = "{{ result | ttl('5s') }}"
```

```shell-session
$ comb get clock.for_5s
05:30:18
$ comb get clock.for_5s    # 2 seconds later
05:30:18
$ comb get clock.for_5s    # 6 seconds later
05:30:23
```

Use it for something cheap to work out on demand, but not worth running when nobody's looking.

## Together

`watch()` and `poll()` combine: `watch('config.json') | poll('10m', 6)` runs when the file changes, and every ten
minutes in case a change was missed. `ttl()` replaces `poll()`, so it doesn't go with it.

The shipped sources are written the same way:

| source | `return` | why |
|---|---|---|
| `git.branch` | `watch('.git/HEAD')` | it only changes when that file does |
| `battery.percent` | `poll('30s', 4)` | there's no file to watch |
| `git.lines_added` | `watch('.', '!.git') \| poll('30s', 4)` | any file in the repository, and a timer as a backstop |

## When a run fails or is slow

More go on the same line: three for when a run doesn't go to plan, and `nearest` for nested roots:

| write | means | default |
|---|---|---|
| `failback('1m', 3)` | after 3 failures in a row, wait a minute before trying again, doubling each time. The last good answer stays meanwhile | `failback('1s', 3)` |
| `timeout('5s')` | a run longer than 5 seconds is stopped, and counts as a failure | 10 seconds |
| `budget('100ms')` | a read that finds the answer out of date waits up to 100 ms for a new one, then answers with the old | 50 ms |
| `nearest` | the nearest root answers alone: nothing falls up from roots above it. [Projects inside projects](./your-own-provider.md#projects-inside-projects-nearest) | off |

All of these can go on one `return` line, together and with `watch()`, `poll()` or `ttl()`, in any order:

```toml
return = '''{{ result
  | watch('~/flag')
  | poll('30s', 4)
  | failback('1m', 3)
  | timeout('5s')
  | budget('100ms') }}'''
```

git's `status` source does the same: it watches the index, polls every minute, backs off after 3 failures, and lets a
read wait 100 ms.

Each is shown below.

### When a run fails: `failback()`

A run fails when its command exits with an error. comb keeps the last good answer and tries again on the next check.
`failback(wait, count)` says what to do when it keeps failing: after `count` failures in a row, wait before trying
again, doubling the wait each time.

This clock fails while a file called `~/broken` exists:

```toml
[daemon.providers.clock.sources.flaky]
data = "{{ exec('test -e ~/broken && exit 1; date +%T') | trim }}"
result.flaky = "{{ data }}"
return = "{{ result | poll('2s', 30) | failback('10s', 2) }}"
```

```shell-session
$ comb get clock.flaky
06:05:59
$ touch ~/broken
$ comb get clock.flaky       # 7 seconds later
06:05:59
$ comb status
  FIELD  VALUE     AGE   POLL  TTL
  flaky  06:05:59  9s×0    5s  ⚠ #02
```

The answer is still the last good one. `⚠ #02` says it has failed twice in a row, and `POLL` counts down to the next
try, 10 seconds after the second failure.

### When a run hangs: `timeout()`

`timeout(time)` stops a run that takes too long, and counts it as a failure:

```toml
[daemon.providers.clock.sources.slow]
data = "{{ exec('sleep 3; date +%T') | trim }}"
result.slow = "{{ data }}"
return = "{{ result | poll('2s', 30) | timeout('1s') }}"
```

```shell-session
$ comb status
  FIELD  VALUE               AGE  POLL  TTL
  slow   timed out after 1s    -    0s  ⚠ #05
```


### How long a read waits: `budget()`

When a read finds an answer that's out of date, it starts a new run and waits a moment for it. If the run finishes in
time, the read gets the new answer. If not, it gets the old one, and the run carries on so the next read has the new
one. `budget(time)` sets that wait.

Two clocks that each take half a second, and expire after 4 seconds:

```toml
[daemon.providers.clock.sources.hasty]
data = "{{ exec('sleep 0.5; date +%T') | trim }}"
result.hasty = "{{ data }}"
return = "{{ result | ttl('4s') }}"

[daemon.providers.clock.sources.patient]
data = "{{ exec('sleep 0.5; date +%T') | trim }}"
result.patient = "{{ data }}"
return = "{{ result | ttl('4s') | budget('1s') }}"
```

Read each, wait 5 seconds so the answer is out of date, and read again:

```shell-session
$ comb get clock.hasty       # at 06:08:15
06:08:10
$ comb get clock.patient     # at 06:08:21
06:08:21
```

`hasty` waited 50 ms, didn't get the new answer in time, and gave the old one. `patient` waited up to a second, so it
gave the new one, half a second later. A prompt wants a short budget: an answer straight away, even a few seconds old.

**Next:** [Caching a template with cache()](./cache.md)

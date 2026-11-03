---
id: "DEC-261104082440"
title: "How does git, the last Rust provider, become config?"
status: accepted
date: 2026-11-04
y-statement: >-
  In the context of moving git to a shipped config file, facing git's need for a
  set environment, tolerated failures and branching logic, we decided for a
  `git` plugin compiled into comb and against shell scripts under exec() or
  jinja argv exec() calls, to achieve no shell on any refresh and one place for
  git's environment, accepting that git's logic is not a copyable config example.
decision-makers: [Joshua Hogendorn]
tags: [providers, plugins, builtins-as-config, phase-7]
supersedes: []
---

# How does git, the last Rust provider, become config?

## Context and Problem Statement

Built-ins are config files (DEC-261007075610), and what jinja cannot do comes
from a plugin (DEC-261007071830). Git is the last built-in in Rust, and the
`Provider` trait goes once it moves. Its `refs`, `diff`, `lineage` and `status`
Sources run git, which needs `GIT_OPTIONAL_LOCKS=0` and prompt guards, has
commands that fail in a healthy repository, and has fallback logic in `lineage`.

## Decision Drivers

* A refresh should not pay for a shell it does not need.
* No Source may be able to forget `GIT_OPTIONAL_LOCKS=0`: `git status` rewriting
  `.git/index` is a self-trigger.
* The `Provider` trait goes.
* Core uses the paths users use.

## Considered Options

* A `git` plugin compiled into comb, called as `plugin('git', <symbol>, root)`
* One shell script per Source under `exec()`, printing `key=value` for `fromini`
* Jinja `exec([...])` calls, with new `env=` and failure-tolerance options
* A `git` plugin shipped as a separate dylib

## Decision Outcome

Chosen option: "compiled-in plugin". The Source stays config and declares its
fields, schedule and root; the plugin is a function that returns its data.
Compiled in rather than a dylib because git is a core use case: nothing extra to
build, sign or find, and it cannot go missing.

Measured on this repository, macOS, release build, load 14–26, 120 alternating
rounds, for one full refresh of all five Sources at p50:

| | Time | Processes |
|---|---|---|
| Rust provider (what the plugin keeps) | 149.6 ms | 10 |
| The same git commands replayed directly | 148.9 ms | 10 |
| Shell prototype, without `fromini` or render | about 168.5 ms | 15, plus forks |

### Consequences

* Good, because no `sh`, `awk` or `fromini` on a refresh, so free-text values
  such as a commit summary need no quoting.
* Good, because gitoxide could later replace commands behind the same symbols.
* Good, because the plugin path gets a second in-house user.
* Bad, because a user cannot copy or edit git's logic as config, only override
  its `result` lines and schedule.
* Bad, because the plugin's file reads are invisible to a warm read's checks, so
  `refs` and `status` must `stat` the files that matter in their own lines.
* Neutral, because a hung git behaves as it does today: the Source's timeout ends
  the run, and its thread and the git process stay until git exits.

## More Information

Supersedes the shell-script recommendation in the phase 7 plan's D2. `head` stays
jinja: it reads two files and runs no process.

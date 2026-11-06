---
id: "DEC-261007080652"
title: "How do templates stay fast when the client computes them outside the daemon's cache?"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of client-rendered templates, facing commands that run on
  every read outside the daemon's cache, we decided for memos stored in the
  daemon via | cache() with watch/ttl invalidation and against daemon-side
  rendering or no caching, to achieve cached per-caller values, accepting
  hash-blind env dependencies.
decision-makers: [Joshua Hogendorn]
tags: [templates, caching, exec, memo, phase-5]
supersedes: []
---

# How do templates stay fast when the client computes them outside the daemon's cache?

## Context and Problem Statement

comb exists so that expensive shell state is computed once and served from
the daemon's cache, kept fresh by polling and file watches. Sources get all
of that.

Templates don't. The client renders them on every read, so they sit outside
the daemon's lifecycle entirely. A template that runs a command,
`{{ exec('kubectl config current-context') }}`, runs `kubectl` on every
`comb status` and every prompt refresh: exactly the cost comb is meant to
remove.

The template can't simply become a Source, because a Source runs in the
daemon and the template needs the caller's environment (`$KUBECONFIG`).

## Decision Drivers

* The value must be computed with the caller's environment.
* The daemon owns lifecycles, not the client: a cached template value must
  expire and refresh by the same polling and file watches as a Source's.
* A warm read must cost one round trip and no command.

## Considered Options

* Memo: the template declares its own caching inline, with filters:

      {{ 'kubectl config current-context'
         | watch(env.KUBECONFIG or '~/.kube/config') | ttl(600)
         | cache(env.KUBECONFIG) | exec() }}

  - `watch(...)` and `ttl(...)` tell the daemon when the memo goes stale,
    using the same file watches and polling a Source uses.
  - `cache(...)` hashes its arguments (or, with none, the command text) and
    looks up the memo. A stored memo is served only if its hash matches, so
    a caller with a different `$KUBECONFIG` misses and renders its own.
  - On a miss, `exec()` runs, the result goes to the caller, and the client
    stores it in the daemon with the hash.
  - When a watch fires or the ttl runs out, the daemon drops the hash; the
    next read misses and renders again.
* The daemon runs the template itself
* No caching: templates always render on read

## Decision Outcome

Chosen option: "memo", because only the client has the caller's
environment, and storing what it rendered in the daemon puts the value back
under the daemon's lifecycle: the same watches, polling and expiry as a
Source's value.

### Consequences

* Good, because `kubectl` runs once per change to `$KUBECONFIG` or the
  kube config file, not once per read.
* Good, because on a miss the caller gets the value before the client stores
  it, so caching never slows the read that pays for it.
* Good, because a failed command is shown but never stored, so the next read
  tries again.
* Bad, because an environment variable that the command uses but that is in
  neither the command text nor `cache(...)`'s arguments is invisible to the
  hash. A caller can then be served a value computed under another
  environment. The template writer must name such variables in `cache(...)`.

## Pros and Cons of the Options

### Memo
* Good, because correct per caller, and cached under the daemon's lifecycle.

### The daemon runs the template
* Bad, because the daemon has its own environment, not the caller's; it
  would need every caller's environment shipped to it (DEC-261007070314).

### No caching
* Good, because nothing to invalidate.
* Bad, because every read runs the command, which is the cost comb removes.

## More Information

Phase 5 spec (exec caching), converged after 9 buddysystem rounds. Where a
memo is stored: DEC-261007074623 (the memo facet).

---
id: "DEC-261007070314"
title: "Environment-dependent fields are rendered by the client"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of fields that depend on the calling shell's environment,
  facing a daemon that cannot see that environment, we decided for
  daemon-stored templates rendered by the client and against shipping or
  registering the env with the daemon, to achieve correct per-caller values
  without env crossing the socket, accepting a template engine in every client.
decision-makers: [Joshua Hogendorn]
tags: [resolution, templates, env, one-tree]
supersedes: []
---

# Environment-dependent fields are rendered by the client

## Context and Problem Statement

Some fields depend on the calling shell's env: `kubecontext.context` follows
`$KUBECONFIG`, `aws.profile` follows `$AWS_PROFILE`. Two shells in the same
directory can need different answers. The daemon is one process per user with
its own env, so it cannot see the caller's.

## Decision Drivers

* Two shells in one directory must each get their own correct value.
* Every surface (comb, SDKs, scanline) must give the same answer for the same env.
* The prompt path must stay one socket round trip.

## Considered Options

* Daemon stores templates; the client renders them with its own env
* Client sends its env with each request; the daemon computes
* Client registers its env once per shell (`context` op); the daemon computes

## Decision Outcome

Chosen option: "client renders", because only the client holds the env, and
rendering where the env lives needs no env on the wire and no per-shell
state in the daemon.

### Consequences

* Good, because per-caller values are correct with no daemon-side session state.
* Good, because one resolver in `libbeachcomber` serves comb and every SDK.
* Bad, because every client embeds minijinja, and values computed client-side
  are uncached unless a template opts into `| cache()`.

## Pros and Cons of the Options

### Client renders
* Good, because env never leaves the caller's process.
* Bad, because template functions must behave the same in every client.

### Env sent per request
* Good, because one renderer, in the daemon, and thin clients.
* Bad, because the whole env crosses the socket on every prompt.
* Bad, because the daemon must cache per env, or recompute every time.

### Env registered per shell
* Good, because env is sent once, not per request.
* Bad, because the daemon holds per-shell state that goes stale on every
  `export` or direnv change, and must be cleaned up when shells exit.

## More Information

Design: `docs/superpowers/specs/2026-09-05-one-tree-resolution-design.md`.
Memos (`| cache()`) are the answer to the uncached cost: phase 5 spec.

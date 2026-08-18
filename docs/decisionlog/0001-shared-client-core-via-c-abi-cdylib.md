---
id: "DEC-0001"
title: "Shared client core via a C-ABI cdylib"
status: accepted
date: 2026-08-15
y-statement: >-
  In the context of six language SDKs and a second in-process consumer,
  facing client-side field resolution that would otherwise be reimplemented
  per language, we decided for exposing the client as a C-ABI cdylib the SDKs
  bind to and against moving resolution into the daemon, to achieve one
  implementation of resolution, protocol and JSON mapping, accepting six SDK
  rewrites and a hard dependency on the native library.
decision-makers: []
tags: [architecture, sdk, ffi, resolution]
supersedes: []
---

# Shared client core via a C-ABI cdylib

## Context and Problem Statement

The six SDKs were designed as thin wrappers around socket management and the
wire protocol, which was a reasonable trade while that was the client's whole
job: six small implementations bought pure-language packages with no native
build.

The client's job has since grown. Client-side field resolution — virtual field
expressions, `env.*`, path expressions — landed in `src/cli/virtual_fields.rs`
inside the CLI binary crate, so `beachcomber-client` and every SDK is locked out
of it. The env-cascade P1 plan recorded this as "SDK parity is out of scope",
deferred to a phase that has not happened.

Two things forced the question now. claude-scanline needs virtual fields
in-process and cannot afford a subprocess per render. Separately, a JSON
conversion bug was found duplicated across four Rust ingestion paths; the same
class exists across six SDKs, each with its own JSON handling, where no shared
test can reach it.

## Decision Drivers

* Resolution logic must exist once — six minijinja evaluators is not a
  maintainable position and no shared test can police them.
* Each SDK is its own JSON ingress with its own type mapping; the C SDK
  hand-rolls a JSON parser.
* The daemon should not become a holder of every shell's environment.
* A bad user expression should degrade one caller, not every shell on the
  machine.
* SDK consumers already require the beachcomber binary, because an SDK with no
  daemon to talk to is useless.

## Considered Options

* Move field resolution into the daemon, keeping SDKs as thin socket wrappers
* Expose the client as a C-ABI cdylib and refactor the SDKs as bindings over it
* Move the evaluator into `beachcomber-client` as a Rust rlib only

## Decision Outcome

Chosen option: "Expose the client as a C-ABI cdylib and refactor the SDKs as
bindings over it", because it puts resolution, protocol framing and JSON mapping
in one implementation rather than relocating the duplication, and because the
daemon alternative requires every caller to ship its environment into a shared
long-lived process.

### Consequences

* Good, because resolution, protocol framing and protocol→JSON mapping exist
  once, so drift in those cannot happen. JSON→language-native mapping remains
  per-binding and still needs fixtures; the divergence surface narrows to one
  layer rather than disappearing.
* Good, because claude-scanline links the client directly and gets resolution
  in-process with no per-render subprocess.
* Good, because the daemon continues never to see a caller's environment or cwd.
* Good, because the library ships with the binary, so language packages stay
  pure and no per-ecosystem native build matrix comes into existence.
* Bad, because it is a rewrite of six SDKs, and a half-migrated state has some
  bindings on the library and some not.
* Bad, because SDKs now hard-require the native library, making an implicit
  dependency explicit and load-bearing.
* Bad, because languages with weak FFI carry a second code path, and PUC Lua
  needs a compiled shim in an otherwise build-free set of bindings.
* Bad, because the C SDK's original justification — usable without a Rust
  toolchain — inverts.

## Pros and Cons of the Options

### Move resolution into the daemon

* Good, because it replicates nothing and the protocol remains the only
  contract.
* Good, because there is no native library to distribute or load.
* Good, because the evaluator is already parameterised on env rather than
  reading the process, so the change would be transport, not semantics.
* Bad, because every connection would ship its environment into a long-lived
  shared process that logs and exposes introspection.
* Bad, because a pathological expression would degrade every shell on the
  machine rather than one caller.
* Bad, because it leaves JSON ingress divergence untouched — six SDKs would
  still each deserialise responses their own way.
* Bad, because which cache slot to read is itself computed from `cwd` and
  `env.*`, so it is a redesign of the resolution model, not a transport swap.

### Expose the client as a C-ABI cdylib

* Good, because one implementation serves every consumer, including future ones.
* Good, because the competence exists in-repo already — shared-library providers
  load via `libloading` with the unsafe FFI isolated in `src/boundaries/`.
* Good, because conformance reduces to testing one implementation plus N
  bindings.
* Bad, because it is the largest of the three changes.

### Move the evaluator into the client as an rlib only

* Good, because it unblocks claude-scanline without touching any binding.
* Bad, because it solves the problem for Rust and leaves the other five SDKs
  where they are, relocating the duplication rather than removing it.

## More Information

`ResolveLive` and the `live.*` namespace, deferred through the env-cascade
plans, are unrelated. They cover live uncached reads at a coordinate the daemon
has not cached, not expression evaluation.

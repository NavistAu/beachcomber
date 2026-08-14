---
id: "DEC-0001"
title: "Shared client core via a C-ABI cdylib"
status: accepted
date: 2026-08-15
y-statement: >-
  In the context of the six language SDKs and a second in-process consumer
  (claude-scanline), facing client-side field resolution that would otherwise be
  reimplemented per language, we decided for exposing beachcomber-client as a
  C-ABI cdylib shipped alongside the comb binary and against moving resolution
  into the daemon, to achieve one implementation of resolution, protocol and
  JSON ingress, accepting a permanent C ABI, a second version surface, and a
  shell-out fallback tier for languages with weak FFI.
decision-makers: []
tags: [architecture, sdk, ffi, packaging, resolution]
supersedes: []
---

# Shared client core via a C-ABI cdylib

## Context and Problem Statement

The six SDKs (c, go, lua, node, python, ruby) were designed as thin wrappers
around socket management and the wire protocol. That was a reasonable trade
while the client's whole job was framing requests: six small implementations of
a line-oriented protocol bought pure-language packages with no native build.

The client's job has since grown. Client-side field resolution — virtual field
expressions, `env.*`, path expressions, the `cache.*` convention — landed in
`src/cli/virtual_fields.rs` (510 lines, minijinja) inside the **CLI binary
crate**. `beachcomber-client` has the socket and protocol and no evaluator, so
every non-CLI consumer is locked out of resolution. The env-cascade P1 plan
recorded this explicitly: "SDK parity is out of scope", deferred to a later
SDK-parity phase that has not happened.

Two things forced the question. claude-scanline needs template widgets, which
are virtual fields, and cannot shell out to `comb` at 5–18ms per render against
a sub-50ms budget. Separately, a JSON conversion bug was found duplicated across
four Rust ingestion paths; the same class exists across six SDKs, each with its
own JSON handling and type mapping, where no shared test can reach it.

## Decision Drivers

* Resolution logic must exist once. Six minijinja evaluators is not a viable
  maintenance position, and no shared test can police them.
* Each SDK is currently its own JSON ingress with its own type-mapping
  decisions; the C SDK hand-rolls `json.c`. This is the same divergence class as
  the four Rust conversions, spread where fixtures cannot catch it.
* The daemon must not become a holder of every shell's secrets.
* A bad user expression should degrade one caller, not every shell on the
  machine.
* Release already builds five targets for the binary; the incremental build cost
  of another artifact from the same jobs is near zero.
* SDK consumers already require the beachcomber binary to be installed, because
  an SDK with no daemon to talk to is useless.

## Considered Options

* Move field resolution into the daemon (a `Resolve` protocol op), keeping SDKs
  as thin socket wrappers
* Expose `beachcomber-client` as a C-ABI cdylib and refactor the SDKs as
  wrappers over it
* Move the evaluator into `beachcomber-client` as a Rust rlib only, leaving the
  other five SDKs without resolution

## Decision Outcome

Chosen option: "Expose `beachcomber-client` as a C-ABI cdylib and refactor the
SDKs as wrappers over it", because it puts resolution, protocol framing and JSON
ingress in one implementation rather than relocating the duplication, and
because the alternative requires the daemon to receive each caller's full
environment.

The cdylib ships in the same package as `comb` — the C SDK is already packaged
as deb and rpm, and `libbeachcomber.pc` already exists — so SDKs load it by name
at runtime and their language packages stay pure. No wheel, gem or rock matrix
comes into existence.

Languages with weak FFI take a documented fallback: Node lists `koffi` as an
optional peer dependency and uses it when present, otherwise shells out to
`comb`; PUC Lua gets a small C shim, LuaJIT uses its built-in `ffi`. The
fallback is sound only because `comb` links the same crate the cdylib exposes,
making FFI and shell-out two transports over one implementation. It is a
documented slow tier at roughly 5ms per call against 0.3ms via socket, not a
silent equivalent.

### Consequences

* Good, because resolution, protocol and JSON type mapping exist once, so
  cross-SDK drift becomes structurally impossible rather than fixture-policed.
* Good, because claude-scanline links `beachcomber-client` as an rlib and gets
  the evaluator in-process, with no new dependency and no per-render fork.
* Good, because the daemon continues to never see a caller's environment or cwd.
* Good, because the incremental build cost is one extra artifact from the five
  release jobs that already exist.
* Good, because language packages remain pure and publish exactly as they do
  now.
* Bad, because a C ABI in a system package is a permanent commitment that
  pre-1.0 "breaking changes are fine" no longer covers; it needs an ABI version
  guard and SONAME versioning from the start.
* Bad, because a loaded lib can be older than the daemon, giving two version
  surfaces where the protocol handshake previously gave one.
* Bad, because Node and PUC Lua carry two code paths instead of one.
* Bad, because SDKs now hard-require the native library to be present, making an
  implicit dependency explicit and load-bearing.

## Pros and Cons of the Options

### Move resolution into the daemon

* Good, because it replicates nothing — SDKs stay thin socket wrappers and the
  protocol remains the only contract.
* Good, because there is no C ABI to own, no SONAME, no runtime library
  resolution, no musl/glibc split, no string-ownership questions.
* Good, because there is one version surface, already handled by the `hello`
  handshake.
* Good, because the evaluator is already parameterised on env — `EvalContext`
  takes an injected `HashMap` rather than reading the process — so the change is
  transport, not semantics.
* Bad, because every connection would ship its environment into a long-lived
  shared process that also writes a log and exposes `introspect`. Sending only
  referenced variables requires knowing the expressions, which puts them back on
  the client.
* Bad, because a pathological expression would degrade every shell on the
  machine rather than one caller.
* Bad, because `env.*` reads currently need no daemon at all; this makes the
  simplest case depend on the most moving parts.
* Bad, because it does not address JSON ingress divergence — six SDKs would
  still each deserialise responses their own way.
* Bad, because which cache slot to read is itself computed from `cwd` and
  `env.*`, so moving resolution moves key computation, env semantics and
  path-phase selection together. It is a redesign of the resolution model, not a
  transport swap.

### Expose `beachcomber-client` as a C-ABI cdylib

* Good, because one implementation covers resolution, protocol and JSON for
  every consumer including future ones.
* Good, because the inbound half of this competence already exists in-repo:
  `libloading`, the unsafe FFI isolated in `src/boundaries/library.rs`, and a
  real cdylib test fixture.
* Good, because the conformance suite reduces to testing one implementation plus
  N bindings, a materially weaker obligation than testing N implementations.
* Bad, because it is a rewrite of six SDKs.
* Bad, because the C SDK's original justification — usable without a Rust
  toolchain — inverts.
* Bad, because it introduces the ABI and version-skew obligations listed above.

### Move the evaluator into `beachcomber-client` as an rlib only

* Good, because it is nearly a file move: `virtual_fields.rs` imports only
  minijinja, serde_json and std, and its single `crate::` reference is inside a
  `#[cfg(test)]` block.
* Good, because it unblocks claude-scanline immediately.
* Bad, because it solves the problem for Rust and leaves the other five SDKs
  exactly where they are — relocating the duplication rather than removing it.

## More Information

The client-side placement of resolution is described in
`docs/canon/field_resolution.md` invariants 1, 2, 6 and 11. Those describe the
current design and are not themselves an argument for retaining it; the reasons
above stand on their own merits, and the canon would be updated to match
whichever model was chosen.

`ResolveLive` and the `live.*` namespace, deferred through the env-cascade P1 and
P2 plans, are unrelated to this decision. They cover live uncached reads at a
coordinate the daemon has not cached, not expression evaluation, and remain
compatible with client-side resolution.

Implementation is specified in `docs/superpowers/specs/2026-08-15-client-abi-and-sdk-refactor.md`.

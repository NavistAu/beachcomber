---
id: "DEC-261007071830"
title: "How do OS-reading built-ins get their data once built-ins are config?"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of converting built-ins to config, facing six providers that
  call libc, we decided for one `sys` plugin compiled into comb through the
  user plugin ABI and against built-in jinja functions, exec of system
  commands, a shipped dylib, or keeping them in Rust, to achieve core
  dogfooding the user's plugin path, accepting an untested loader step.
decision-makers: [Joshua Hogendorn]
tags: [providers, plugins, builtins-as-config, phase-6]
supersedes: []
---

# How do OS-reading built-ins get their data once built-ins are config?

## Context and Problem Statement

Built-ins are becoming TOML files of jinja Sources, so that core uses the
same path users do. `hostname`, `load`, `uname`, `uptime`, `user` and
`network` get their data from `libc` (`gethostname`, `getloadavg`, `uname`,
`sysctl`, `getpwuid_r`, `getifaddrs`). Jinja cannot make those calls.

## Decision Drivers

* Core must not have a privileged path that users can't use.
* One way to do each thing: a user extends comb through plugins, so core does too.
* A refresh should stay at about 10 µs, without a process spawn.
* The macOS/Linux split should not leak into every shipped file.

## Considered Options

* A `sys` plugin compiled into comb, called through the plugin ABI
* Built-in jinja functions (`hostname()`, `loadavg()`)
* The same plugin shipped as a separate dylib
* `exec()` of system commands (`hostname`, `sysctl`)
* Keep these six as Rust Sources

## Decision Outcome

Chosen option: "compiled-in plugin", because core then calls the OS exactly
as a user's plugin would. The only step it skips is loading the file, and a
real dylib in the test suite covers that step.

### Consequences

* Good, because the plugin ABI gets an in-house user, so it can't quietly rot.
* Good, because no extra file to build, sign, package or find on `plugin.path`.
* Bad, because the static plugin skips `dlopen`, so the loader is covered
  only by a test plugin, not by core.

## Pros and Cons of the Options

### Compiled-in plugin
* Good, because same ABI, same JSON, same call spelling as user plugins.
* Bad, because it is verbose (`plugin('sys', 'loadavg')`) until plugins can
  register named functions.

### Built-in jinja functions
This was the first proposal and the simplest: a thin Rust function per OS
call, registered straight into the template engine.
* Good, because least code, fastest, and the nicest spelling.
* Bad, because core would get a magic extension point users don't have:
  users add OS access through plugins, core through private functions.
* Bad, because it makes two ways to do one thing, and the plugin path would
  lose its only in-house user. Pushed instead to conform to the plugin spec.

### Shipped dylib
* Good, because the purest dogfooding: core loads exactly as users do.
* Bad, because of a build per target, macOS signing, and packaging; a missing
  file breaks `hostname`.

### exec / keep Rust
* exec: no new Rust, but a spawn per refresh and platform branches per file.
* Keep Rust: no work, but the `Provider` trait survives for six providers.

## More Information

Phase 6 spec, section "Ruling 2026-10-07: OS-reading built-ins are a plugin".
Depends on the built-ins-as-config decision. CLAUDE.md, "One way to do each
thing".

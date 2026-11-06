---
id: "DEC-261007075610"
title: "Are built-in providers written in Rust, or as config like a user's own?"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of comb's built-in providers, facing built-ins and user
  providers travelling different paths, we decided for shipping built-ins as
  config files and against Rust built-ins with config overrides, to achieve
  comb dogfooding its users' config and plugin paths, accepting about 10 µs
  per refresh.
decision-makers: [Joshua Hogendorn]
tags: [providers, config, builtins-as-config, phase-6]
supersedes: []
---

# Are built-in providers written in Rust, or as config like a user's own?

## Context and Problem Statement

comb's built-in providers (git, mise, aws, …) are Rust code. A user's own
providers are config: TOML files of jinja Sources. So the two travel
different paths, and only the user's path depends on config working well.

Built-ins could stay Rust with config able to tune them, or be shipped as
config files written exactly the way a user writes one.

Shipping them as config means comb itself depends on the paths users depend
on: config, Sources, and plugins. A problem in those paths becomes our
problem, so it gets noticed and fixed rather than shipped. The built-in
files also become working examples a user can copy when writing their own
providers, and to a lesser extent their own plugins.

## Decision Drivers

* Overhead: config must not make comb measurably slower.
* comb should use the same paths its users do.

## Considered Options

* Built-ins stay Rust; config can override their settings
* Built-ins are config files; Rust provides only the engine functions and
  filters they call (`exec`, `read`, `lup`, `url`, `plugin`, …)

## Decision Outcome

Chosen option: "built-ins are config", because it makes comb depend on the
same paths its users do, and the overhead measured as negligible: on this
machine, with minijinja 2.19 in a release build, about 10 µs per refresh,
about 7 µs per read, and 1 ms once at start. For comparison, a socket round
trip is about 200 µs and a `git` spawn 2–5 ms.

### Consequences

* Good, because a weakness in config, Sources or plugins shows up in comb's
  own providers first.
* Good, because every built-in is a working example for users.

## Pros and Cons of the Options

### Rust with config overrides
* Bad, because comb never exercises the path users rely on, so problems in
  it go unnoticed.

### Built-ins are config
* Good, because comb dogfoods config and plugins, and users get examples.

## More Information

Ruled 2026-09-24 (session `5e738ce3`). Phase 6 spec. Related: the decision on
OS-reading built-ins (DEC-261007071830).

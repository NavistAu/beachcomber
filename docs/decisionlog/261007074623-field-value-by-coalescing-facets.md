---
id: "DEC-261007074623"
title: "How do we decide a field's value when a Source, a put, a template and a memo can each supply one?"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of fields that a Source, a put, a template and a memo can
  each supply, facing a rules engine that had grown hard to reason about, we
  decided for each writing to its own facet and reads coalescing memo > template >
  put > source, and against the decision engine, to achieve one simple
  rule, accepting nothing of note.
decision-makers: [Joshua Hogendorn]
tags: [resolution, facets, phase-1]
supersedes: []
---

# How do we decide a field's value when a Source, a put, a template and a memo can each supply one?

## Context and Problem Statement

A field's value can come from four places: a Source, a put, a template, and a
template's memo. They can overlap on the same field. Something has to decide
which value a caller gets.

The design was heading toward a decision engine: rules about who may read and
write a field, when and where. Each new case added another rule, and the
whole became hard to reason about. That difficulty was the warning sign: the
approach needed simplifying, not more rules.

## Decision Drivers

* The rules must be simple enough to hold in your head.
* A Source, a put, a template and a memo should each be able to change
  without knowing about the others.

## Considered Options

* A decision engine: rules for who may read and write a field, when and where
* Facets: each writes to its own facet whenever it likes; a read coalesces
  the facets in a fixed order of preference

## Decision Outcome

Chosen option: "facets", because the whole decision engine collapses to one
coalesce, in this order of preference: memo > template > put > source.
A put sits below the template so that a template can enrich or act on a put
value, reading it with `comb.*` and choosing to merge or replace; without a
template, a put still wins over the Source.

### Consequences

* Good, because nothing checks whether a write is allowed; every write just
  lands in its own facet.
* Good, because removing a value lets the next facet in the order show through.

## Pros and Cons of the Options

### A decision engine
* Good, because it was already designed and reviewed (exec-caching spec).
* Bad, because every new case needs another rule, and the rules interact.

### Facets
* Good, because the only logic is the order.

## More Information

Phase 1 spec, "Ruling 2026-10-07: layers, not ownership".

---
id: "DEC-261007081820"
title: "Where does the template for a client-computed field live?"
status: accepted
date: 2026-10-07
y-statement: >-
  In the context of client-computed fields, facing templates that need keys
  without taking field names, we decided for a facet on the provider
  (`example:template.theme`) and against a reserved provider, a reserved key,
  or metadata subkeys on each field, to achieve a general extra dimension for
  field metadata, accepting a new key syntax.
decision-makers: [Joshua Hogendorn]
tags: [keys, facets, templates, one-tree]
supersedes: []
---

# Where does the template for a client-computed field live?

## Context and Problem Statement

Some fields are computed by the client: `example.theme` is rendered from a
jinja string each time it is read. That string is the field's template. The
daemon stores it and hands it to the client, so it needs a key.

The obvious key, `example.template.theme`, puts the templates among
`example`'s own fields. Then `example` can never have a field called
`template`.

Spreadsheets show where mixing these leads. Excel keeps a cell's value, its
formula and its formatting in one place, and the result is the familiar
mess, such as typed values silently turning into dates. A template is not the
field's value. It is information about the field, so it belongs on a
separate dimension of that field, not mixed in among the values.

## Decision Drivers

* No part of a provider's field names may be reserved for comb.
* A template stays attached to its provider.
* The same mechanism should carry other information about a field later.

## Considered Options

* A reserved provider named `template`: `template.example.theme`
* A reserved key inside each provider: `example.template.theme`
* Metadata as subkeys of each field: `example.theme.template`
* A facet on the provider: `example:template.theme`, a second dimension of
  `example` with the same shape as its fields

## Decision Outcome

Chosen option: "a facet on the provider", because it reserves no names: the
template sits beside `example`, not inside it, and `example` keeps every field
name for itself. The separator is `:` because it is not valid jinja, so a
facet key can't be confused with an expression inside a template, and it
needs no quoting in a shell.

### Consequences

* Good, because `comb get example:template` returns all of `example`'s
  templates at once, the same way `comb get example` returns all its fields.
* Good, because it is a general solution, not just a home for templates.
  Facets make the tree N-dimensional: any kind of information about a field
  gets its own dimension with the same shape as the fields. The separate
  places a field's value can be written (put, memo, template, source) and
  the cache lifecycle data (`:cache`) all became facets with no new syntax.

## Pros and Cons of the Options

### A reserved provider named `template`
* Good, because no new key syntax.
* Bad, because no provider can be called `template`, and a provider's
  templates live away from the provider.

### A reserved key inside each provider
* Good, because no new key syntax, and the templates stay with the provider.
* Bad, because no provider can have a field called `template`.

### Metadata as subkeys of each field
* Good, because the information sits directly on the field it describes.
* Bad, because fields can themselves be objects, so `foo.bar.baz` becomes
  ambiguous: a real field, a `baz` metadata entry on `foo.bar`, or a key
  inside a JSON value at `foo` that was expanded.

### A facet on the provider
* Good, because no names reserved anywhere, and no ambiguity about what a
  key addresses.

## More Information

Decided 2026-09-05 (session `6acf4a0e`). One-tree design spec, §2 and §3.9.
Later facets: DEC-261007074623 (put, memo, template and source facets).

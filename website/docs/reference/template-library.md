---
sidebar_position: 5
title: Template Library
---

# Template Library

The filters and functions a template can call. Templates are jinja (minijinja), in a client template leaf, `-f`,
`comb eval`, a `status` row, an SDK `get`/`eval`, and a Source's lines. minijinja's builtins (`default`, `join`,
`select`, `round`, `format`, ...) are there as well; this page is the library comb adds.

## Rules for every item

- **One name, one form.** An item is a filter or a function, with no alias. `merge` and `exec` keep both forms.
- **Absent in, absent out.** Given `none` or undefined, every filter returns *undefined*. `| default(x)` then fires
  without a second argument, and `~` joins it as nothing. This covers minijinja's builtins too (`none | upper`,
  `missing | length`). The exceptions exist to handle absence or to render it: `default`/`d`, `bool`, `tojson`,
  `pprint`, `safe` and `escape`/`e`.
- **Empty is absent for the parse and path filters.** `'' | fromjson`, `'' | fromini`, `'' | basename` and
  `'' | dirname` are undefined.
- **Text filters take a string.** A number or a bool is its text; a list or a map is an error.
- **Bad input is an error the user sees**, written `name(): why`, for example
  `fg(): invalid colour "nope": expected #rrggbb hex`.

## Where each item runs

Each item has a tier, and the tier says where it is available.

| tier | reads | client | Source lines | provider lines, `return` decorators |
|---|---|---|---|---|
| pure | its arguments | yes | yes | yes |
| io | files | yes | yes | no |
| exec | processes | when `exec` is allowed | yes | no |
| daemon | the network, plugins | refused: `url() runs only in a Source's lines` | yes | no |

A provider's lines run on every read, so they use the pure items only. An item that is not available there is a load
error that names it.

## Items

### Data

| item | meaning |
|---|---|
| `a \| merge(b, ...)`, `merge(a, ...)`, `merge(list)` | deep merge, later wins; a `none` leaf removes; an absent operand adds nothing, and with nothing left the result is undefined |
| `v \| pluck(path)` | deep get by path in the key grammar (`a.b[0]`, `[-1]`, `['k.y']`); any miss is undefined |
| `obj \| map('filter', arg...)` | applies a filter to each value of an object and keeps the keys; on a list it is jinja's `map` |

### Paths

| item | meaning |
|---|---|
| `p \| basename` | the last component, as POSIX `basename`: `'/a/b/'` is `b`, `'/'` is `/` |
| `p \| dirname` | all but the last component, as POSIX `dirname`: `'/a/b/c.txt'` is `/a/b`, `'c.txt'` is `.` |

### Files and parsing

| item | meaning |
|---|---|
| `p \| read` | the file's text; a missing file is undefined. A relative path is against the render's directory and `~` is the render's `HOME` |
| `lup(name, all=, files=, dirs=, follow=, git=, root=)` | the nearest path called `name` at or above the render's directory; a miss is undefined |
| `s \| fromjson`, `s \| fromini` | text to data; text that is not JSON or INI is an error |
| `exec(cmd, ...)`, `cmd \| exec()` | runs a command |

### Text and terminal

| item | meaning |
|---|---|
| `s \| truncate(n)` | the first `n` characters, then `...` |
| `s \| fg(colour)`, `s \| bg(colour)` | a `#rrggbb` colour as a terminal escape; empty text gives empty text |
| `s \| bold`, `s \| dim` | the SGR attribute |

With `NO_COLOR` set to something in the render's `env`, the colour filters emit no escapes.

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
| `s \| width` | display width in terminal columns: `'💭 x' \| width` is `4` |
| `s \| pad(n, align='left')` | padded with spaces to `n` columns; `align` is `left`, `right` or `center` (`center` puts the floor of half the gap on the left); wider text, or a negative `n`, is returned unchanged |
| `s \| truncate(n, end='…')` | cut to `n` columns with `end` counted inside `n`: `'abcdef' \| truncate(4)` is `abc…`; an `end` wider than `n` is dropped, and a wide character that would straddle the cut is dropped whole |
| `s \| fg(colour)`, `s \| bg(colour)` | the text in a foreground or background colour (below) |
| `s \| bold`, `dim`, `italic`, `underline`, `invert` | SGR attributes 1, 2, 3, 4 and 7 |
| `hyperlink(url, text)` | `text` as an OSC 8 terminal link to `url`; absent `text` is undefined, an absent `url` leaves the text unlinked, and a `url` that is not a string or holds a control character is an error |

**Display width.** Wherever an item measures text it counts terminal columns: East Asian wide and emoji-presentation
characters are 2, combining marks are 0, and escape sequences (CSI such as colours, and OSC such as `hyperlink`) are 0.
`truncate` never cuts an escape sequence and keeps every one, so a reset or a link closer after the cut survives. There
is no character-count variant.

Every styling filter closes with a full reset (`\e[0m`), emits nothing for empty text, and nests as it is written:
`x | fg(c) | bg(d) | bold`.

**Colours.** A colour is one of:

- a name: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, and each with a `bright_` prefix
  (`bright_red`);
- an integer 0-255, an index in the 256-colour palette;
- a string `#rrggbb`, 24-bit.

Nothing else parses: not a bare `rrggbb`, not `#rgb`, not `rgb()`. An unknown colour is an error
(`fg(): unknown colour "nope": ...`), checked before the text, so an empty or absent subject does not hide a typo. A
colour of `none` applies no colour.

**Depth.** The colour is drawn at the depth the render's terminal variables say: a `COLORTERM` of `truecolor` or
`24bit` is 24-bit, else a `TERM` containing `256color` is 256 colours, else 16. A colour deeper than the terminal is
mapped to the nearest one it has. A caller that wants 24-bit colour passes `COLORTERM` (and `TERM`) in the render's
environment; `comb eval` and `comb get` pass the process environment. Without them the render is 16-colour.

With `NO_COLOR` set to something in the render's environment, no item emits an escape sequence, `hyperlink` included.

---
sidebar_position: 7
title: Macros
---

# Macros

A macro is a named piece of template text with parameters. It is the one way to reuse text in a template.

## Within one text

`{% macro %}` defines a macro and `{% call %}` calls one with a body. Both work in every line comb renders: a client
template, a Source's `root`, `data`, `result` and `return` lines, and a provider's `template`, `result` and `return`
lines.

```jinja
{%- macro badge(text, colour) -%}
  {{- (" " ~ text ~ " ") | fg(colour) -}}
{%- endmacro -%}
{{- badge(git.branch, "#a6e22e") -}}
```

- A macro returns a string: what its body prints.
- A macro sees the names of the text it is defined in, as well as its parameters.
- In a call block, the macro reads the block's body as `caller()`:

  ```jinja
  {%- macro boxed() -%}[{{ caller() }}]{%- endmacro -%}
  {%- call boxed() -%}{{ git.branch }}{%- endcall -%}
  ```

## Libraries

A macro library is a text of macro definitions, declared on the client side of the configuration:

```toml
[client.providers.scanline.macro]
usage = '''
{%- macro usage(p, tp) -%}
  {{- "red" if p > tp else "green" -}}
{%- endmacro -%}
'''
```

- It is `scanline:macro.usage`, in the `macro` facet: `comb get scanline:macro` gives every library of `scanline`,
  and `comb get scanline:macro.usage` gives the one text.
- `<name>` follows the name rule: no `.`, `:`, `[`, `]` or `*`, and no leading `/`, `~` or `<`. The value is a string.
- A library holds `{% macro %}` definitions, top-level `{% set %}`s and imports of other libraries. It prints nothing:
  text, a `{{ }}` or a `{% call %}` outside a `{% macro %}` is an error.
- Each file is a layer: a later file replaces one library and keeps its siblings.
- `comb put scanline:macro.usage '"…"'` writes one at a directory or at `--global`, and the nearest directory's
  library wins. A put is checked as configuration is.
- `macro` under `[daemon.providers.<p>]` is `unknown key`: macros are client templates.

`comb check config` lists a library's problems:

```text
[FAIL] local    config(~/.config/beachcomber/config.toml): [client.providers.scanline] macro.usage: a macro library prints nothing: its text, {{ }} and {% call %} belong inside a {% macro %}
[WARN] local    config(~/.config/beachcomber/config.toml): [client.providers.scanline] macro.rate: unknown macro library 'scanline:macro.theme' (no layer declares it)
```

- A library that does not compile, or prints, is an error, and it is not seeded.
- One that imports a library no layer declares is a warning: a later `put` may declare it.

`comb status -a`, or `--filter facet=macro`, shows one row per library per layer, its SOURCE the file or `put`.
`comb check config --origins` lists each `client.providers.<p>.macro.<name>` with the files it shadows.

## Importing a library

A client template imports a library by its address, written as a string:

```jinja
{%- from "scanline:macro.usage" import usage -%}
{{- usage(scanline.rate.five_hour.used_pct, 50) -}}
```

```jinja
{%- import "scanline:macro.rate" as rate -%}
{{- rate.row("five_hour", "5H") -}}
```

- A macro returns a string. A module import (`import … as`) also exposes the library's top-level `set`s, with their
  types: `rate.lw` is a number.
- An imported library renders in the importing template's context: it sees `env`, `comb`, the providers by name and
  the importer's own names. A provider it reads is read for the importer, as if the importer read it.
- A library may import another. Each is read once per read of a value.
- A client template is any text the client renders: a `[client.providers.<p>] template` leaf, a `put` to
  `<p>:template`, `comb eval`, `-f`, a `comb status` row, and an SDK's `get` and `eval`.
- A library is something a value depends on: `comb watch` follows it, so editing the library re-renders the watch.
  It is text, like a template, so its age is not the value's.
- `exec()` in a macro runs only where the importing render may run it.

What fails:

| What | Error |
|---|---|
| An import whose name is computed, not written as a string | a compile error, before anything renders |
| An address that is not a library's, `<p>:macro.<name>` | a compile error |
| A library nothing answers at the address | `<key>: scanline:macro.rate: no macro library there`; a group leaves that value out |
| Something other than a string at the address | a configuration error |
| An error inside a library | its own words and place: `… (in scanline:macro.rate:3)` |
| Two libraries that import each other | `recursion limit exceeded` |
| A library that reads the template importing it | the cycle error, as for any template |

A Source's or a provider's line cannot import: macros are client templates, and the line is a load error,
`macros are client templates: a Source's line cannot import`. It may still define and call a macro within itself.

## Typed after statements

A text of statements and comments, then its only `{{ }}`, with nothing but whitespace between, keeps that
expression's type, on every line:

```jinja
{%- set ns = namespace(d={}) -%}
{%- for name in ["five_hour", "seven_day"] -%}
  {%- set ns.d = ns.d | merge({name: scanline.rate[name].used_pct}) -%}
{%- endfor -%}
{{- ns.d -}}
```

- That is the map `ns.d`, not its text, so no `tojson`/`fromjson` round trip is needed.
- A macro's, a call's or a block `set`'s body does not count: it prints nothing where it stands.
- The statements print nothing. One that does, a `{% call %}`, is an error: `a typed line's statements print
  nothing`.

## What is refused

`{% include %}`, `{% extends %}` and `{% block %}` are refused on every line:

```text
{% include %} is refused: text is reused with a macro
```

- In configuration it is a load error.
- In a `put` to `<p>:template` or `<p>:macro` the put is refused.
- In `comb eval` and `-f` it is a compile error, exit 2.

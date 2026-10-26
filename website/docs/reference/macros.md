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

## What is refused

`{% include %}`, `{% extends %}` and `{% block %}` are refused on every line:

```text
{% include %} is refused: text is reused with a macro
```

- In configuration it is a load error.
- In a `put` to `<p>:template` or `<p>:macro` the put is refused.
- In `comb eval` and `-f` it is a compile error, exit 2.

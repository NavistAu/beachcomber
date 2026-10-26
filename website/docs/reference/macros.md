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

## What is refused

`{% include %}`, `{% extends %}` and `{% block %}` are refused on every line:

```text
{% include %} is refused: text is reused with a macro
```

- In configuration it is a load error.
- In a `put` to `<p>:template` the put is refused.
- In `comb eval` and `-f` it is a compile error, exit 2.

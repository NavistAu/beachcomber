---
sidebar_position: 8
title: Template namespaces
---

# Template namespaces: constants and composed values

A provider can be made of **templates** alone: no Source, nothing for the daemon to run. A template is a jinja
expression stored under `[client.providers.<p>] template`; the daemon keeps it, and the client (`comb get`, `comb eval`,
every SDK) renders it on each read with the caller's environment and directory. Use one for constants, for a value
from the environment, and for a value composed from other providers.

[Templates](../concepts/templates.md) explains the language; this page shows the common uses.

## Constants

```toml
# ~/.config/beachcomber/conf.d/theme.toml
[client.providers.theme.template]
accent = "#5fafff"
width = "{{ 40 }}"
mode = "{{ env.THEME_MODE or 'dark' }}"
```

```sh
comb get theme -f json
```

```json
{
  "accent": "#5fafff",
  "mode": "dark",
  "width": 40
}
```

```sh
THEME_MODE=light comb get theme.mode
```

```
light
```

- Text with no `{{ }}` is its own literal text: `accent` is a string.
- A leaf that is exactly one `{{ expr }}` keeps the expression's type: `width` is the number `40`.
- `env` is the reader's environment, so each shell can see its own `mode`.

Nested tables are groups: `[client.providers.theme.template.colours]` makes `theme.colours.<leaf>`, and `comb get
theme.colours` returns the group as an object.

## Composed values

A template can read other providers. `<provider>.<field>` is that field as `comb get` would answer it (templates
included); `comb.<provider>.<field>` is the raw cached value.

```toml
[client.providers.prompt.template]
project = "{{ pkg.name ~ '@' ~ pkg.version if pkg.version else none }}"
crate = "{{ crate.name ~ ' v' ~ crate.version if crate.version else none }}"
half = "{{ theme.width // 2 }}"
```

`pkg` and `crate` are the Sources from [From files](./from-files.md). In a Node project:

```sh
cd ~/app/src && comb get prompt -f json
```

```json
{
  "half": 20,
  "project": "my-app@2.4.0"
}
```

In a Rust crate, `crate` is present and `project` is not:

```json
{
  "crate": "widget v0.3.1",
  "half": 20
}
```

A leaf whose single expression gives `none` is **absent**, never `""`: it is left out of the group, and a read of it is a
miss (`comb --strict get prompt.project` exits 1 outside a project). Write the guard as one expression, as above. Text
around a tag always makes a string, so `"{{ crate.name }} v{{ crate.version }}"` would render ` v` where there is no
crate.

## Writing a template at runtime

`comb put` writes a template into the `:template` facet. It takes effect on the next read and lasts until the daemon
stops:

```sh
comb put --global 'prompt:template.mode' '"{{ theme.mode | upper }}"'
comb get prompt.mode
```

```
DARK
```

The raw text is readable the same way: `comb get prompt:template` prints the expressions unrendered.

## Commands in templates

A template can run a command with `exec()`, but only on a client that allows it (`comb get`, `comb eval`, an SDK
client constructed with `exec` on), and it runs on every read. Pipe it through `cache()` so its value is remembered
until a watched file changes or a ttl ends; the shipped `kubecontext` template does this. See
[Caching a template](../reference/template-library.md#caching-a-template). A command that should run on a schedule, once
for every reader, belongs in a [Source](./from-a-command.md).

## A constant in the daemon

A provider's own `result` line can also be a literal. It is computed in the daemon and needs no Source:

```toml
[daemon.providers.consts]
result = "{{ {'region': 'ap-southeast-2', 'retries': 3} }}"
```

```sh
comb get consts -f json
```

```json
{
  "region": "ap-southeast-2",
  "retries": 3
}
```

A provider line sees no `env` and does no IO, so prefer client templates unless the value must be the same for every
reader.

## Sharing pieces

Formatting reused across several templates belongs in a macro library, `[client.providers.<p>] macro.<name>`. See
[Macros](../reference/macros.md).

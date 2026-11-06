---
sidebar_position: 6
title: From files
---

# A provider from files

When the value you want is in a file, read the file in the Source's `data` line. `read`, the parse filters and `stat`
run inside the daemon, so a run starts no process and costs microseconds rather than milliseconds. Watch the file, and
the value refreshes as soon as it changes.

## A project's `package.json`

```toml
# ~/.config/beachcomber/conf.d/pkg.toml
[daemon.providers.pkg.sources.main]
root = "{{ lup('package.json') | dirname }}"
data = "{{ 'package.json' | read | fromjson }}"
result.name = "{{ data.name }}"
result.version = "{{ data.version }}"
return = "{{ result | watch('package.json') | nearest }}"
```

- `root` finds the nearest `package.json` at or above the directory being read and uses its directory. Each project gets
  its own instance.
- `data` reads `package.json` (a relative path is against the root) and parses it.
- `return` re-runs the Source when `package.json` changes. `nearest` makes a directory with no `package.json` above it a
  miss rather than falling back to `~`.

With `~/app/package.json` holding `{ "name": "my-app", "version": "2.4.0" }`:

```sh
cd ~/app/src/deep && comb get pkg
```

```
name=my-app
version=2.4.0
```

## Other formats

Pick the parse filter for the file:

| File | `data` line |
|---|---|
| JSON | `"{{ 'package.json' \| read \| fromjson }}"` |
| TOML | `"{{ 'Cargo.toml' \| read \| fromtoml }}"` |
| YAML (one document) | `"{{ '~/.kube/config' \| read \| fromyaml }}"` |
| INI or `key=value` | `"{{ 'setup.cfg' \| read \| fromini }}"` |
| One value | `"{{ '.node-version' \| read \| trim }}"` |

A Cargo crate's name and version, with `pick` keeping two keys of the `[package]` table:

```toml
[daemon.providers.crate.sources.main]
root = "{{ lup('Cargo.toml') | dirname }}"
data = "{{ 'Cargo.toml' | read | fromtoml }}"
result = "{{ data.package | pick('name', 'version') }}"
return = "{{ result | watch('Cargo.toml') | nearest }}"
```

```sh
cd ~/rs && comb get crate -f json
```

```json
{
  "name": "widget",
  "version": "0.3.1"
}
```

A version file, watched so an edit shows on the next read:

```toml
[daemon.providers.nodever.sources.main]
root = "{{ lup('.node-version') | dirname }}"
data = "{{ '.node-version' | read | trim }}"
result.version = "{{ data }}"
return = "{{ result | watch('.node-version') | poll('300s') | nearest }}"
```

```sh
cd ~/proj/sub && comb get nodever.version     # 20.11.0
echo 22.1.0 > ~/proj/.node-version
comb get nodever.version                      # 22.1.0
```

`poll('300s')` beside `watch` is a fallback in case a filesystem event is missed.

## A file's size and age

`stat` describes a file without reading it: `{size, mtime, kind, mode}`, or nothing when it is missing.

```toml
[daemon.providers.hosts.sources.main]
data = "{{ '/etc/hosts' | stat }}"
result.size = "{{ data.size }}"
result.mtime = "{{ data.mtime }}"
return = "{{ result | watch('/etc/hosts') }}"
```

```sh
comb get hosts
```

```
mtime=1777558367
size=213
```

## Missing files

`read` and `stat` of a missing file give nothing, and a filter given nothing gives nothing, so a field whose file is
missing is absent rather than an error. Guard a line when you want a value anyway:
`"{{ data.version or 'unknown' }}"`.

## The functions

`read`, `lup`, `stat`, `glob`, `realpath` and the parse and regex filters are in the
[Template Library](../reference/template-library.md#files-and-parsing). The same filters work in client templates, so a
value cheap enough to compute on every read can also be a [template](./template-namespaces.md) with no Source at all.

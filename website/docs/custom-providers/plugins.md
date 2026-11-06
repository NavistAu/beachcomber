---
sidebar_position: 10
title: Shared-library plugins
---

# A provider from a shared-library plugin

A **plugin** is a shared library (`.dylib` on macOS, `.so` on Linux) whose functions a Source's `data` line calls with
`plugin(name, symbol, args...)`. The call runs inside the daemon, with no process started, so it suits work that runs
often and costs too much as an `exec()`: compiled parsing, a vendor SDK, a local socket protocol. A plugin is only a
function; the Source around it still declares the fields, the schedule and the root.

## Write one

Every symbol has the same C signature: it takes the call's arguments as a JSON array and returns a JSON value.

```c
// hello.c
#include <stdio.h>

/* plugin('hello', 'greet', 'world')  ->  {"greeting": "hello, world"} */
static char out[256];

const char *greet(const char *input) {
    char name[128];
    /* input is the arguments as a JSON array: ["world"] */
    if (sscanf(input, "[\"%127[^\"]\"]", name) != 1) return NULL;
    snprintf(out, sizeof out, "{\"greeting\": \"hello, %s\"}", name);
    return out;
}
```

Build it into the default plugin directory, `~/.config/beachcomber/plugins/`, named `lib<name>`:

```sh
mkdir -p ~/.config/beachcomber/plugins
cc -shared -o ~/.config/beachcomber/plugins/libhello.dylib hello.c          # macOS
cc -shared -fPIC -o ~/.config/beachcomber/plugins/libhello.so hello.c       # Linux
```

## Call it from a Source

```toml
# ~/.config/beachcomber/conf.d/hello.toml
[daemon.providers.hello.sources.main]
data = "{{ plugin('hello', 'greet', 'world') }}"
result = "{{ data }}"
return = "{{ result | poll('1h') }}"
```

```sh
comb get hello.greeting
```

```
hello, world
```

The returned JSON comes back parsed, so `data` is an object here. A `NULL` return fails the run, and the log says so:

```
WARN beachcomber::run: Source 'hellobad.main' failed: hellobad.main: data: plugin(): greet in hello: returned NULL
```

As with any Source, a failed run keeps the last good value.

## Where libraries are found

`plugin('hello', ...)` looks for `libhello.dylib` or `libhello.so` in each directory of `[daemon.plugin] path`, in
order. A name containing `/` is a path to the library itself (absolute, or starting `~/`).

```toml
[daemon.plugin]
path = ["~/.local/lib/beachcomber", "/opt/beachcomber/plugins"]
```

A library is loaded on its first call and stays loaded for the daemon's life: after rebuilding one, run `comb kill` so
the next daemon loads the new file. Changing `[daemon.plugin]` restarts the daemon.

A plugin runs inside the daemon with its permissions. Put only libraries you trust on the path.

## The rules a symbol follows

- Signature `const char *symbol(const char *input)`. `input` is a NUL-terminated JSON array of the arguments.
- Return a NUL-terminated UTF-8 JSON value owned by the library, valid until the next call into it; a static buffer is
  enough, and there is no free function. Return `NULL` for failure.
- Never unwind: a panic or exception escaping the function aborts the daemon.
- The daemon calls one library from one thread at a time.

The full contract is in [Provider Development: Plugins](../internals/provider-development.md#8-plugins).

## The plugins compiled in

Two plugins are built into `comb` and reached the same way: `sys`, for what only the operating system can say, and
`git`, for repositories. The shipped `hostname`, `load`, `battery`, `network` and `git` providers are Sources over them,
and your own Sources can call them too:

```toml
[daemon.providers.box.sources.load]
data = "{{ plugin('sys', 'loadavg') }}"
result.one = "{{ data.one }}"
return = "{{ result | poll('10s') }}"
```

`comb get box.one` prints the one-minute load average. The names `sys` and `git` are reserved: a `libsys` or `libgit` on
the plugin path is never loaded. Their symbols are listed in
[Provider Development](../internals/provider-development.md#8-plugins).

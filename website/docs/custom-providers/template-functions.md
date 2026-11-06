---
sidebar_position: 3
title: Template functions
---

# Template functions

Templates are Jinja, so they come with Jinja's own filters: `upper`, `default`, `join`, `round` and the rest. comb
adds its own, for the jobs a prompt or a source actually has. This guide walks through them by job. The
[template library](../reference/template-library.md) lists every one with all its options.

`comb eval` renders a template on the spot, so you can try each of these as you read.

## Where a function can run

Most functions run anywhere: in a template you read, in `-f`, in `comb eval` and in a source's lines. A few are
limited, because of what they cost:

| function | in a template you read | in a source |
|---|---|---|
| reading files: `read`, `lup`, `stat` | yes | yes |
| running a command: `exec` | yes, with `comb get`; off by default in libraries | yes |
| fetching a URL: `url`, and calling a plugin: `plugin` | no | yes |
| keeping an answer: `cache`, `watch`, `ttl` | yes | no: a source keeps its answer already |

Anything that runs a command or fetches a URL is slow next to everything else. In a source it runs once and is kept;
in a template it runs on every read unless you [cache it](../concepts/cache.md).

## Find and read files

`lup` looks for a file going up from where you are, the same way the cascade does, and gives its path. `read` reads
a file, and `fromjson`, `fromtoml`, `fromyaml` and `fromini` turn its text into values:

```shell-session
$ cd ~/code/shop/web
$ comb eval "{{ lup('package.json') }}"
/home/you/code/shop/package.json
$ comb eval "{{ (lup('package.json') | read | fromjson).version }}"
1.5.0
$ comb eval "{{ lup('package.json') | dirname | basename }}"
shop
```

A file that isn't there reads as nothing, not an error. `stat` gives a file's size, kind and modification time without
reading it.

## Run a command

`exec` runs a command and gives what it printed. Give it a string to run through the shell, or a list to run directly
with no shell involved:

```shell-session
$ comb eval "{{ exec('uname -s') | trim }}"
Darwin
$ comb eval "{{ exec(['echo', 'hi']) | trim }}"
hi
```

`trim` takes off the newline the command printed at the end. A command that fails is an error you'll see; add
`or 'fallback'` after it if failing is normal.

## Fetch from the web

`url` fetches a URL and gives the response body. It runs only in a source, so it runs once and is kept, and never
holds up a prompt. [From an HTTP API](./from-an-http-api.md) has a full example with headers and authentication.

## Shape text

```shell-session
$ comb eval "{{ '~/code/shop/web/src' | shorten_path }}"
~/c/s/w/src
$ comb eval "{{ 'feature/login-page' | truncate(10) }}"
feature/l…
$ comb eval "{{ git.branch | pad(8) }}|"
main    |
```

`truncate` and `pad` count terminal columns, so wide characters and emoji line up.

## Colour and style

`fg` and `bg` set the text and background colour, by name (`red`, `bright_blue`), number (0 to 255) or `#rrggbb`.
`bold`, `dim`, `italic` and `underline` set the style:

<pre className="terminal"><span className="terminal-prompt">$ </span>comb eval "&#123;&#123; git.branch | fg('cyan') &#125;&#125; &#123;&#123; 'dirty' | fg('red') | bold &#125;&#125;"{'\n'}<span className="terminal-cyan">main</span> <b className="terminal-red">dirty</b></pre>

The colour depth follows your terminal (`COLORTERM`, then `TERM`), and `--color never` or `NO_COLOR` turns colour off.

## Numbers, sizes and time

```shell-session
$ comb eval "{{ 3725 | duration }}"
1h2m
$ comb eval "{{ (now() - 11520) | ago }}"
3h12m ago
$ comb eval "{{ now() | strftime('%H:%M') }}"
09:41
$ comb eval "{{ 1288490188 | bytes }} {{ 56789 | compact }}"
1.2G 57k
```

`ramp` picks a word, icon or colour by where a number falls, which is handy for a battery or a load average:

```shell-session
$ comb eval "{{ battery.percent | ramp([20, 50], ['low', 'mid', 'high']) }}"
high
```

## Icons and bars

`icon` gives a [Nerd Fonts](https://www.nerdfonts.com/cheat-sheet) glyph by name, and `file_icon` the glyph for a
kind of file. You need a Nerd Font in your terminal to see them:

```text
{{ icon('dev-git') }} {{ git.branch }}
{{ file_icon('package.json') }} {{ pkg.version }}
```

`bar` draws a fixed-width bar, for things like a battery or disk usage:

```text
{{ bar(10, {'value': battery.percent / 100, 'colour': 'green'}) }}
```

## When a value is missing

A missing value is empty, and every comb filter passes empty through as empty, so a template doesn't break when
something isn't there. Say what to show instead with `or`, or with `default`:

```shell-session
$ comb eval "{{ kubecontext.context or 'no cluster' }}"
no cluster
$ comb eval "{{ missing.thing | default('none set') }}"
none set
```

## Keeping an answer

`cache`, `watch` and `ttl` keep a template's answer so a command in it doesn't run on every read.
[Caching a template with cache()](../concepts/cache.md) explains them.

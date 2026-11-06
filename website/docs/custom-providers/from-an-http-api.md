---
sidebar_position: 5
title: From an HTTP API
---

# A provider from an HTTP API

Call `url()` in a Source's `data` line. The daemon makes the request itself, with no `curl` process, and caches the fields
you pick out of the response.

## A public endpoint

```toml
# ~/.config/beachcomber/conf.d/github-status.toml
[daemon.providers.status.sources.main]
data = "{{ url('https://www.githubstatus.com/api/v2/status.json') | fromjson }}"
result.indicator = "{{ data.status.indicator }}"
result.description = "{{ data.status.description }}"
return = "{{ result | poll('60s') | failback('5s', 3) }}"
```

```sh
comb get status
```

```
description=All Systems Operational
indicator=none
```

`url()` returns the response body as text; `| fromjson` parses it. `result.<field>` lines pick fields out of the parsed
body, and `result = "{{ data.rate }}"` would make one whole object the provider's fields instead:

```toml
[daemon.providers.gh_rate.sources.main]
data = '''{{ url('https://api.github.com/rate_limit', headers={'Accept': 'application/json'})
  | fromjson }}'''
result = "{{ data.rate }}"
return = "{{ result | poll('5m') }}"
```

```sh
comb get gh_rate
```

```
limit=60
remaining=58
reset=1793898342
used=2
```

For a body that is not JSON, leave out `fromjson` and use the text: `result.body = "{{ data | trim }}"`.

## `url()` arguments

`url(address, method='GET', headers={}, body=none, timeout='10s')`

| argument | meaning |
|---|---|
| `address` | an absolute `http://` or `https://` URL |
| `method` | `GET`, `POST`, `PUT` or `PATCH` |
| `headers` | a map of header names to string values |
| `body` | a string; needs a method other than `GET`. Build JSON with `\| tojson` |
| `timeout` | how long the request may take, a duration |

A POST with a token and a JSON body:

```toml
[daemon.providers.echo.sources.main]
data = '''{{ url('https://httpbin.org/anything',
    method='POST',
    headers={
      'Authorization': 'Bearer ' ~ env.API_TOKEN,
      'Content-Type': 'application/json',
    },
    body={'q': env.REGION} | tojson,
    timeout='5s') | fromjson }}'''
result.method = "{{ data.method }}"
result.sent = "{{ data.json }}"
return = "{{ result | poll('1h') }}"
```

```sh
comb get echo -f json
```

```json
{
  "method": "POST",
  "sent": {
    "q": "eu"
  }
}
```

`url()` runs only in the daemon. In a client template it is an error, `url() runs only in a Source's lines`.

## Keep secrets in the env file

`env` in a Source's lines is the daemon's environment, not your shell's. A daemon started on demand inherits whatever
environment started it, so put the variables a Source needs in the **env file**, which the daemon loads at startup:

```sh
# ~/.config/beachcomber/env
# KEY=VALUE, one per line; blank lines and #comments are ignored; values may be quoted.
API_TOKEN="replace-me"
REGION=eu
```

```sh
chmod 600 ~/.config/beachcomber/env
comb kill      # the next read starts a daemon that has loaded it
```

The token stays out of the configuration files, and `comb check config` reads the same file, so it checks with the
variables the daemon would have. An unset variable renders empty: `'Bearer ' ~ env.API_TOKEN` then sends `Bearer ` and
the API refuses it. To keep the file elsewhere, set `[daemon] env_file = "~/.secrets/comb.env"`.

A secret manager can write the file for you, for example `op read "op://Vault/comb/env" > ~/.config/beachcomber/env`.

## When a request fails

A non-2xx status, a connection error, a body that is not UTF-8 and a timeout all fail the run. The last good value stays
in the cache and readers keep getting it. The log says what happened, with the query string left out:

```
WARN beachcomber::run: Source 'broken.main' failed: broken.main: data: url(): https://api.github.com/this-does-not-exist: status 404
```

`failback(interval, count)` on `return` sets how the Source backs off: after `count` failures in a row, retries are
spaced from `interval`, doubling. The run as a whole is cut off at `[daemon] timeout` (default `10s`), or at
`| timeout('30s')` on the Source's `return`.

Poll an API no faster than it changes and no faster than its rate limit allows: `poll('86400s')` for a daily figure.

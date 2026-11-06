---
sidebar_position: 1
title: Overview
---

# Guides

Each guide here solves one job, with a complete example you can paste in and adapt. They build on
[How comb works](../concepts/values-and-names.md), so read that first if sources, puts and templates are new.
[A value's life](../concepts/a-values-life.md) explains how a value is kept current.

## Pick the guide for your value

| I want to show… | Guide |
|---|---|
| text shaped, coloured or worked out with comb's own functions | [Template functions](./template-functions.md) |
| the output of a command or script | [From a command](./from-a-command.md) |
| something from a file, like a version in `package.json` | [From files](./from-files.md) |
| something from a web API | [From an HTTP API](./from-an-http-api.md) |
| something another program knows, like a deploy status | [Data you push](./data-you-push.md) |
| a fixed value, or one built from other values | [Template namespaces](./template-namespaces.md) |
| a built-in value, but checked more often, shown differently, or not at all | [Change a built-in provider](./change-a-shipped-provider.md) |
| something only native code can get cheaply | [Shared-library plugins](./plugins.md) |

## The cheapest way first

If more than one guide fits, pick the one that does the least work per update: reading a file costs least, then a web
request, then running a command. A program that already knows the value and puts it costs nothing to keep fresh.

---
sidebar_position: 4
title: Templates
---

# Templates

A template is text with values dropped in. Templates are written in [Jinja](https://jinja.palletsprojects.com/en/stable/templates/),
the template language used by Ansible, Flask and many others, so anything you know from Jinja works here: `{{ }}`
for values, `{% if %}` and `{% for %}`, filters with `|`. comb runs it with
[MiniJinja](https://docs.rs/minijinja/latest/minijinja/syntax/index.html) and adds
[its own filters and functions](../reference/template-library.md) for prompts and status bars.

Anything inside `{{ }}` is worked out; everything else is printed as it is. `comb eval` renders a template, so you
can try them out:

```shell-session
$ cd ~/code/shop
$ comb eval '{{ git.branch }} · {{ team.name }}'
main · checkout
```

## Reading other values

A template reads any value by its full name, `git.branch` or `team.name`, from the directory you're in, with the
cascade working as usual. It gets the same answer `comb get` would, templates included.

`comb.` in front of a name reads only what the daemon has stored: values from sources and puts, without templates.

```shell-session
$ comb eval '{{ aws.profile }}'
default
$ comb eval '{{ comb.aws.profile }}'

```

`aws.profile` is a template, so it's not stored, and `comb.aws.profile` is empty. `comb.` is for a template that
wants the stored value underneath itself, such as a put or a source's value it overrides.

[Sources](./your-own-provider.md#reading-other-values) read other values the same way.

## Decide what to show

`if` shows something only when a condition holds, and `or` gives a fallback for a value that's missing:

```shell-session
$ comb eval '{{ battery.percent }}%{{ " charging" if battery.charging }}'
80%
$ comb eval '{{ kubecontext.context or "no cluster" }}'
no cluster
```

A missing value is empty, so `or` is how you say what to show instead.

## Change how a value looks

A `|` passes a value through a **filter**, which changes it:

```shell-session
$ comb eval '{{ hostname.short | lower }}'
laptop
$ comb eval '{{ "~/code/shop/web/src" | shorten_path }}'
~/c/s/w/src
```

Colour filters print the escape codes your terminal turns into colour:

<pre className="terminal"><span className="terminal-prompt">$ </span>comb eval '&#123;&#123; git.branch | fg("cyan") &#125;&#125; &#123;&#123; "dirty" | fg("red") | bold &#125;&#125;'{'\n'}<span className="terminal-cyan">main</span> <b className="terminal-red">dirty</b></pre>

There are filters for colour, dates and durations, paths, numbers, JSON and more; the
[template library](../reference/template-library.md) lists them all.

## Your shell's environment

`env.NAME` reads an environment variable from the shell that's reading:

```shell-session
$ comb eval '{{ env.AWS_PROFILE or "default" }}'
default
$ AWS_PROFILE=staging comb eval '{{ env.AWS_PROFILE or "default" }}'
staging
```

This works because a template is worked out by `comb` itself, in your shell, as you read. Two terminals with
different `AWS_PROFILE`s each get their own answer.

## Where templates go

You've already used templates in two places: `comb eval`, and `-f` on `comb get`, where a namespace's values can be
used by their short names (`comb get git -f '{{ branch }}'`). The next page shows the third: giving a template a
name in your configuration, so it becomes a value anyone can read.

**Next:** [Configuration](./configuration.md)

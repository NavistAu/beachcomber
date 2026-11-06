---
sidebar_position: 3
title: See it working
---

# See it working

Open a few terminals in the same repository, and maybe a tmux pane or two. They all show the branch. Now ask comb
what it's holding:

```shell-session
$ comb status
  PROVIDER  FIELD     SOURCE          VALUE   PATH          AGE    POLL
  battery   percent   source(state)   80      ~             12s    18s
  git       branch    source(head)    main    ~/code/shop   41s      -
  git       dirty     source(status)  false   ~/code/shop   41s    19s
  …
```

There is one `git.branch` for `~/code/shop`, however many prompts are reading it. `AGE` says it was worked out 41
seconds ago: since then, every prompt has been handed that same answer instead of running git again.

Now change something:

```shell-session
$ git switch -c feature
$ comb status
  git       branch    source(head)    feature   ~/code/shop    0s      -
```

comb noticed the change and worked the branch out once more. Every prompt picks up `feature` the next time it
draws.

That's the whole idea: the slow work happens once, when something changes, instead of every time anything asks.
The more terminals, panes and tools you have reading the same things, the more it saves.

## When to look again

You don't need to start, stop or look after comb. It starts itself on the first `comb` command, and values nobody
reads are dropped after a while. If something looks wrong, [Troubleshooting](./troubleshooting.md) covers the
usual causes.

**Next:** to make comb show more, or show it your way, learn [how comb works](../concepts/values-and-names.md).

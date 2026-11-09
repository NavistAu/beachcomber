# beachcomber Roadmap

Open work only. What shipped is `CHANGELOG.md`; how things got fixed is git history.

---

### High Priority (next)

- [ ] **Every documented claim has a test.** The docs-against-code walk is done (about thirty statements corrected
      by running the binary against them; see `CHANGELOG.md` Unreleased → Docs). What it did not do: for each claim in
      `website/docs/reference/` and `website/docs/internals/`, name the test that asserts it, and add one where none
      does. Goal: the next 0.6.1-class docs-said-X / code-did-Y bug fails a test before a user finds it.

### General

- [ ] **Trust for config and plugins not supplied by the user.** Today comb reads config only from the user's own
      config directory (`defaults/`, `config.toml`, `conf.d/`), and a `put` needs a process already running as the
      user, so every `exec()` sits behind a trust decision the user already made. A project file (below) and
      third-party plugins break that (vim's `exrc` problem). The rule: **an untrusted file may read, but not act**:
      what only reads a value (the `Pure`, `Clock`, `Io` and `Memo` tiers of `libbeachcomber::library::Tier`) runs
      as the user's own lines do, and what acts on something else needs trust — the `Exec` tier (`exec()`) and the
      `Daemon` tier (`url()`, whose request carries data out in its URL, and `plugin()`, which runs a library's
      code), which fail as `exec() is disabled in this context` does. The tier is declared beside each function in
      `library/mod.rs`, so a new function says which side it is on where it is added. Trust is per file and per content, in the style of `direnv allow` and hush: a sha256 of the file
      keyed by its resolved path, an edit untrusted until approved again, an unedited copy in a git worktree trusted
      as its main checkout's is. Not a global `exec` on/off switch (considered and rejected 2026-10-07). The review
      is comb's own: render what the file would change (its leaves, as `comb status --tree` lays out layers) and
      what it would `exec()`. hush (`src/eval.rs`, `is_trusted` / `record_trust`) and ambit (`config.py`, `trusted`)
      keep the same registry; extract it as a small Rust crate when this starts (the registry, the worktree rule,
      the private-file write, helpers a review renders with), with ambit calling it or keeping its own 20 lines.
- [ ] **A project config file in a directory.** Config today is global, so there is no way to shape what comb
      feeds one project's prompt: in a tool-heavy directory it drowns in `mise` tools. A file in the project
      (`.combrc`, `.beachcomber.toml`?) would carry that directory's config: templates, Sources and settings that
      apply at it and below, nearer beating farther as values do. Conceptually close to a `put` at a directory,
      which already gives one project its own value or template (`comb p --path ~/ws/proj …`), but its own
      mechanism: a config layer found on the directory chain and watched for edits, not a put. Open: its
      discovery, how it composes with the user's config, and what of `[daemon]` it may set. Needs trust (above),
      since the file comes from whoever wrote the repo. Acceptance: a project's prompt shows an allowlist of its
      `mise` tools instead of every one.
- [ ] **Precompute a directory's values before the prompt asks.** Entering a directory with a project file, or
      one direnv binds an environment to, starts cold: the first prompt waits for every Source it reads to run.
      The daemon could warm them ahead of the read: when a project file is trusted or edited, and when direnv
      exports an environment for a directory (ingest it, and key the env-derived entries by `(path, that env)`),
      so `cd` lands on cache hits. This bends "a read of a missing value runs whatever provides it; nothing
      pre-computes", deliberately and for prompt latency; what it warms, and for how long, are the design.
- [ ] **A memo that survives switching environments.** A client template piped through `cache(env.X)` is a cache
      hit for the environment it last saw (`kubecontext` over `$KUBECONFIG`), but the memo holds one hash per leaf
      per directory, so a shell that alternates `$KUBECONFIG` or `$AWS_PROFILE` in one directory misses on each
      switch. Keep a few hashes per leaf, or key the memo by its inputs.

- [ ] **A `put` over a callee does not rerun its callers.** A Source that reads another is run again when the
      callee's input changes or the callee stores a new value, but a `put` over the callee's node changes what the
      caller would read without counting. Decide whether a put should count, and on which reads.
- [ ] **A callee that changes during its caller's run.** If the callee changes after the caller read it but before the
      caller's record is stored, the record looks newer than the change and the caller is not rerun until its next run
      (marked `shortcut:` in `src/provider/source_call.rs`). Fix by stamping each call with the value it saw.

### CLI Ergonomics

- [ ] **Response metadata on stderr for plain-text output.** Plain-text `comb get` prints the value alone, so a
      shell caller cannot see envelope metadata (`age_ms`, `stale`, whether a refresh is still running after the
      read-time validity check timed out — `docs/superpowers/specs/2026-09-23-read-validity-design.md`). A flag
      could write that metadata to stderr while stdout stays the bare value. Flag name and stderr format open.
- [ ] `comb inspect <path>` (working name) — dump every applicable provider's fields for the given directory in one
      shot. Every provider evaluated against the path; single command replaces "ask every provider one
      at a time". Useful for "what does beachcomber know about this dir right now?" as a diagnostic, onboarding, and
      integration-authoring tool. Format-aware output (json/text/human). Naming bikeshed: `inspect`, `dump`, `snapshot`,
      `all`, `info`.
- [ ] `comb ui` — live TUI as a sister binary. `watch comb status` works but can't render nested-object values and has
      no interactivity; a proper TUI could add controls (force refresh, inspect lifecycle state, drill into a value).
      Keep TUI dependencies out of `comb` itself: separate `comb-ui` binary, dispatched git-style (`comb ui` execs
      `comb-ui` from PATH). Fits the dependency-audit / comb-paranoid goals. Open questions: workspace member vs
      separate repo; what the control surface actually is. Related to `comb inspect` above — that's the one-shot form,
      this is the live form.

### Additional Providers

- [ ] **`exec()` that reads an exit status.** Any non-zero exit fails the render (`libbeachcomber/src/exec.rs`
      `run`): the error names the command, the status and stderr, and the stdout is dropped. A command whose status
      is the answer has no clean way to say it: `ssh-add -l` exits 1 with no keys and 2 with no agent, `klist -s`
      speaks only through its status, `grep` uses 1 for no match. Shipped files work around it
      in the shell (`kubecontext`: `… 2>/dev/null || true`), which loses which failure it was. Proposed: a
      `check=false` keyword, as Python's `subprocess.run`, returning `{stdout, stderr, status}` for any exit and
      failing only on a timeout, an oversized stdout or a spawn error; without it, as today. Wants it before the
      `ssh`, `gpg` and `kerberos` providers below.

- [ ] `ssh` provider — `keys_loaded` (count of loaded keys), `agent_running` (bool). Global, poll. Useful for prompt
      indicators before push/deploy operations.
- [ ] `gpg` same as ssh, is the agent running, key count, etc
- [ ] `kerberos` provider — active ticket state. Global, poll.
- [ ] `docker` provider — running container count, current context. Support Docker, OrbStack, and Podman. Global, poll.
  - [ ] this could also be project path specific in the case of docker-compose files perhaps?
- [ ] `brew` provider — count of outdated packages. Global, poll (daily). Extend pattern to other package managers:
      `apt`, `dnf`/`yum`, `apk`, `pacman`. Shipping one file per manager wants the item below, so a machine without
      `apt` has no `apt` provider rather than one whose Source fails on every poll.
- [ ] **`enabled` as a jinja expression.** `[daemon.providers.<p>] enabled` is a bool today. Let it be a line that
      renders to one, so a shipped provider switches itself off where its tool is missing:
      `enabled = "{{ 'apt' | which }}"`. Rendered at load and on each reload, with the daemon's environment and the
      IO tier (`glob`, `stat`, `read`) but no `exec()`; needs a `which` filter (a `$PATH` lookup, none when absent).
      Open: a tool installed while the daemon runs stays off until the next reload, unless the `$PATH` directories
      are watched. `comb check providers` says why: `disabled (enabled rendered false)`.
- [ ] have claude-scanline `put` the usage stats into a static namespace every time it gets hit, so they read back as
      cached values.

### Performance Validation

- [ ] Hyperfine benchmarks — before/after comparisons for tmux status bar refresh and p10k prompt render with
      beachcomber vs native tools. Publish results on the website.
- [ ] **Skip realpath on a warm read.** `query::Dir::requested` canonicalises every request path (about a third of the
      daemon's CPU per warm read in a 2026-11 profile). A memo is only correct if it sees any ancestor renamed or
      replaced by a symlink; a `stat` of the target cannot, and an `lstat` per component costs ~80% of realpath. It
      needs invalidation from directory events on the ancestors. `normalise_follows_the_tree_as_it_changes` pins
      the behaviour a memo must keep.
- [ ] **Event-driven validity for a warm read.** `start_for_read` → `record_state` stats every recorded input on each
      read (and, for a Source that read others, each callee's state as well), and the climb re-derives its instances (`kept_root`, `next_instance`) per request. A fast path for an
      already-validated leaf means invalidating on file events instead of stat'ing on read, without a staleness
      window.
- [ ] **Track the suite's speed in CI.** The 10s target holds on one 12-core Mac, and CI's runners are slower and
      vary, so neither a fixed number nor a cutoff works there. Keep each run's nextest junit report (total and per
      test), compare it with recent runs on the same runner type, and flag a run or a test that is well over its own
      history, so a slower suite shows up in review instead of being found later on a laptop.
- [ ] Docker test containers — controlled environments for each integration target (oh-my-tmux, p10k, starship, etc.)
      with both stock and beachcomber-integrated configs. Enables reproducible perf tuning, CI benchmarks, and
      side-by-side demos for upstream adoption PRs.

### Documentation

- [ ] We should do a llm/agent md page in the website that gives a compact token efficient summary of the tool for llm
      purposes. this is different to llm.txt which is more for training datasets and marketing.
  - [ ] Is there any possible utility in making a beachcomber skill/plugin?
- [ ] Fun examples — copy-paste config Source recipes for the website. Show beachcomber as a general-purpose cached
      data layer, not just a devtools daemon. Practical: local weather, quote of the day, today's Wordle number,
      countdown to next One Piece episode, ISS overhead pass, Hacker News top story, Spotify current track, Home
      Assistant entity state (front door lock, thermostat, lights via HA REST API). Weird/fun (not all committed, pick
      the best):
  - Mercury Retrograde deploy guard — a Source polling an astrology API with `url()`, git pre-push hook refuses to push to
    production during retrograde. `mercury_yolo_mode = true` to override.
  - Don't Deploy On Friday — same pattern, simpler astrology. Block or warn on pushes to production branches after
    Thursday EOD.
  - Physical deploy confidence gauge — combine git.dirty, git.ahead, load, battery, time-since-commit into a 0-100
    score. Pipe via `comb watch` to an Arduino servo gauge on your desk. Zones: "YOLO", "Maybe", "Safe", "It's Friday
    Don't".
  - Vibe check emoji — single emoji in prompt computed from weather, day of week, battery, git state, load. Monday +
    rain + dirty repo + low battery = 💀. Friday + sunshine + clean = 🏖️. Full decision tree as a flowchart.
  - Office thermostat wars — poll HA thermostat, alert in tmux when someone changes it with who and by how much. Rolling
    sparkline of temperature battles over the day.
  - Banana scale — compute physical length of terminal scrollback in bananas (lines x font size x approximate character
    height ÷ 17.78cm). Display as a literal string of 🍌 emoji. Utterly useless, deeply satisfying.
  - Claude token odometer — `wc -l` of Claude Code conversation history from the last 24 hours. Show in tmux/prompt as a
    rolling count of how much you've been talking to your AI pair programmer today.
  - Magic 8-Ball deploy oracle — config Source that caches a random 8-ball response ("Outlook not so good", "Signs
    point to yes", etc.) and reshakes at random intervals. Git pre-push hook checks `comb get 8ball.sentiment` and
    blocks deploys on negative readings. Show the current reading in tmux. The universe has opinions about your release
    schedule.
- [ ] Timeseries / sparkline guide — document how to use `comb put` to accumulate rolling timeseries data (e.g.
      service response times, error rates, CPU samples) and render sparkline/histogram status tickers in tmux or
      prompts. Cover append-to-array patterns, TTL-based expiry, and how to structure the data for consumers that render
      sparklines (e.g. oh-my-tmux, tmux-sparkline, unicode block characters).

### Project Quality

- [ ] Dependency audit — evaluate all Rust dependencies, document each one on the website with rationale for inclusion.
      Remove anything not strictly necessary.
  - [ ] Consider a comb-paranoid build that cuts down featureset significantly and dependency chain. depends on how
        painful this would be to do and maintain. For example, dropping all the formatting tools drops the minijinja
        dep. Maybe core dependencies go back to as old a version as makes sense to reduce supply chain attack.
- [ ] Publish the LuaRocks rock under a `navist` account (LuaRocks has no orgs) instead of `jhogendorn`; then point
      the website (`Hero.tsx`) and README links at `luarocks.org/modules/navist/libbeachcomber`.

# beachcomber Roadmap

Open work only. What shipped is `CHANGELOG.md`; how things got fixed is git history.

---

### High Priority (next)

- [ ] **Every documented claim has a test.** The docs-against-code walk is done (about thirty statements corrected
      by running the binary against them; see `CHANGELOG.md` Unreleased → Docs). What it did not do: for each claim in
      `website/docs/reference/` and `website/docs/internals/`, name the test that asserts it, and add one where none
      does. Goal: the next 0.6.1-class docs-said-X / code-did-Y bug fails a test before a user finds it.

### General

- [ ] **Trust rules for config and plugins not supplied by the user.** Today comb reads config only from the user's
      own config directory (`defaults/`, `config.toml`, `conf.d/`), and a template `put` needs a process already
      running as the user, so every `exec()` sits behind a trust decision the user already made. Two planned features
      break that: project-level comb config read from the directory being entered (vim's `exrc` problem), and
      third-party plugins. Both need a trust registry in the style of `direnv allow` — what is trusted, keyed how, and
      what an untrusted source may still do — not a global `exec` on/off switch (considered and rejected 2026-10-07).

- [ ] **direnv-driven precompute of env-derived ("P2") values into the cache.** The env+cascade design (per-shell env
      vars are the final override on top of the daemon's path-cached values; cascades are jinja, first-non-empty +
      transforms, evaluated client-side) resolves the "env selects a _non-default source_" cases — a per-shell
      `$KUBECONFIG`, a non-default `$AWS_PROFILE`'s config region, `$CLOUDSDK_ACTIVE_CONFIG_NAME` — by a **live,
      uncached read** on override (correct, but not cache-fast). Because direnv binds env to a path, a direnv
      integration could let the daemon precompute and cache these per-path env-derived values, turning those live reads
      back into cache hits where direnv is in play. Deferred until the env/cascade work lands; needs a way to ingest a
      directory's direnv-exported env and key the affected cache entries by `(path, that env)`.
- [ ] **Per-directory customization.** In tool-heavy directories the prompt drowns in `mise` tool/version output —
      there's no way to shape what comb feeds a given project's prompt. Two candidate mechanisms: (a) a computed
      field (e.g. `tools_prompt`) that filters the `mise` tools through an allowlist and the prompt pulls instead of the
      raw tool list — the env/cascade computed-fields work gives the expression machinery, but config is global today;
      (b) a project config file (`.beachcomber.toml`?) carrying per-dir allowlists / template definitions — new
      discovery and trust surface, since config would come from arbitrary repos. Cross-references the env/cascade item
      above. The mise prompt-spam case is the acceptance test.

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

- [ ] `ssh` provider — `keys_loaded` (count of loaded keys), `agent_running` (bool). Global, poll. Useful for prompt
      indicators before push/deploy operations.
- [ ] `gpg` same as ssh, is the agent running, key count, etc
- [ ] `kerberos` provider — active ticket state. Global, poll.
- [ ] `docker` provider — running container count, current context. Support Docker, OrbStack, and Podman. Global, poll.
  - [ ] this could also be project path specific in the case of docker-compose files perhaps?
- [ ] `brew` provider — count of outdated packages. Global, poll (daily). Extend pattern to other package managers:
      `apt`, `dnf`/`yum`, `apk`, `pacman`.
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

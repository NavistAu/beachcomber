# beachcomber Roadmap

Open work only. What shipped is `CHANGELOG.md`; how things got fixed is git history.

---

### High Priority (next)

- [ ] **Canon ↔ code ↔ docs reconciliation (correctness pass).** The 0.6.1 socket-resolution and Node-dist bugs were
      both canon/docs-said-X / code-did-Y divergences, caught only by a partial README+website audit — which itself
      shipped errors needing a corrective second pass (`02115b2f`). Do a systematic walk of the project's line of truth
      (`docs/canon/{cache-lifecycle,provider_source,singleton}.md` → test suite → code → `docs/protocol-spec.md` /
      `docs/architecture.md` / `docs/performance.md` / `docs/status_ttl.md` / `docs/versioning.md` → README → website).
      For each canon claim verify: (a) a test asserts it, (b) code matches the test, (c) docs/website describe the
      actual behavior. Log every divergence in `docs/known-issues.md` with either a fix or an explicit, reasoned
      deferral. Goal: surface the next 0.6.1-class bug before a user does. Theme chosen 2026-05-29 in the post-0.6.1
      retro.

### General

- [ ] **direnv-driven precompute of env-derived ("P2") values into the cache.** The env+cascade design (per-shell env
      vars are the final override on top of the daemon's path-cached values; cascades are jinja, first-non-empty +
      transforms, evaluated client-side) resolves the "env selects a _non-default source_" cases — a per-shell
      `$KUBECONFIG`, a non-default `$AWS_PROFILE`'s config region, `$CLOUDSDK_ACTIVE_CONFIG_NAME` — by a **live,
      uncached read** on override (correct, but not cache-fast). Because direnv binds env to a path, a direnv
      integration could let the daemon precompute and cache these per-path env-derived values, turning those live reads
      back into cache hits where direnv is in play. Deferred until the env/cascade work lands; needs a way to ingest a
      directory's direnv-exported env and key the affected cache entries by `(path, that env)`.
- [ ] **`watch` support for computed fields.** The env/cascade design adds client-side **computed fields** (jinja
      expressions over `env.*` + `provider.field`, evaluated in the CLI). v1 wires them into `comb get` and `comb eval`
      only. `comb watch <computed.field>` is deferred — users will reasonably expect `watch python.version` to behave
      like `get python.version`, so this needs a client-side re-evaluation loop driven by the underlying daemon fields'
      changes (and a story for `env.*`, which the daemon can't watch). Pick up after the env/cascade P1 lands.
- [ ] **A path-expression compile error is swallowed to `None`.** `path_expr::evaluate_path` returns
      `Option<String>`, where `None` means "empty/falsy result ⇒ the global slot" (canon `field_resolution.md`
      §"Path resolution"). A source that fails to compile takes the same `None` — so a typo in a config `path =`
      silently collapses the provider to the global slot instead of reporting the syntax error. Both forms now
      compile (`{{ }}` and bare), which removes the most common way to hit this, but the wart stands: the return
      type needs to distinguish "no path" from "bad expression", and every caller (`comb get`'s resolution layer,
      `bc_resolve`'s path-expression arm, the conformance runners) needs to surface it.

### CLI Ergonomics

- [ ] `comb inspect <path>` (working name) — dump every applicable provider's fields for the given directory in one
      shot. Globals + path-scoped providers evaluated against the path; single command replaces "ask every provider one
      at a time". Useful for "what does beachcomber know about this dir right now?" as a diagnostic, onboarding, and
      integration-authoring tool. Format-aware output (json/text/human). Naming bikeshed: `inspect`, `dump`, `snapshot`,
      `all`, `info`.

### Additional Providers

- [ ] `ssh` provider — `keys_loaded` (count of loaded keys), `agent_running` (bool). Global, poll. Useful for prompt
      indicators before push/deploy operations.
- [ ] `gpg` same as ssh, is the agent running, key count, etc
- [ ] `kerberos` provider — active ticket state. Global, poll.
- [ ] `docker` provider — running container count, current context. Support Docker, OrbStack, and Podman. Global, poll.
  - [ ] this could also be project path specific in the case of docker-compose files perhaps?
- [ ] `brew` provider — count of outdated packages. Global, poll (daily). Extend pattern to other package managers:
      `apt`, `dnf`/`yum`, `apk`, `pacman`.
- [ ] have claude-scanline make a virtual provider that injects the usage stats in as cached values every time it gets
      hit.

### Performance Validation

- [ ] Hyperfine benchmarks — before/after comparisons for tmux status bar refresh and p10k prompt render with
      beachcomber vs native tools. Publish results on the website.
- [ ] Docker test containers — controlled environments for each integration target (oh-my-tmux, p10k, starship, etc.)
      with both stock and beachcomber-integrated configs. Enables reproducible perf tuning, CI benchmarks, and
      side-by-side demos for upstream adoption PRs.

### Documentation

- [ ] Audit and consider restructuring according to diataxis
- [ ] We added a jinja style formatter, thats used in a number of places. we need to ensure our docs are comprehensive
      about its usage.
- [ ] We should do a llm/agent md page in the website that gives a compact token efficient summary of the tool for llm
      purposes. this is different to llm.txt which is more for training datasets and marketing.
  - [ ] Is there any possible utility in making a beachcomber skill/plugin?
- [ ] Fun examples — copy-paste script provider recipes for the website. Show beachcomber as a general-purpose cached
      data layer, not just a devtools daemon. Practical: local weather, quote of the day, today's Wordle number,
      countdown to next One Piece episode, ISS overhead pass, Hacker News top story, Spotify current track, Home
      Assistant entity state (front door lock, thermostat, lights via HA REST API). Weird/fun (not all committed, pick
      the best):
  - Mercury Retrograde deploy guard — HTTP provider polling an astrology API, git pre-push hook refuses to push to
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
  - Magic 8-Ball deploy oracle — script provider that caches a random 8-ball response ("Outlook not so good", "Signs
    point to yes", etc.) and reshakes at random intervals. Git pre-push hook checks `comb get 8ball.sentiment` and
    blocks deploys on negative readings. Show the current reading in tmux. The universe has opinions about your release
    schedule.
- [ ] Timeseries / sparkline guide — document how to use `comb store` to accumulate rolling timeseries data (e.g.
      service response times, error rates, CPU samples) and render sparkline/histogram status tickers in tmux or
      prompts. Cover append-to-array patterns, TTL-based expiry, and how to structure the data for consumers that render
      sparklines (e.g. oh-my-tmux, tmux-sparkline, unicode block characters).

### Project Quality

- [ ] Dependency audit — evaluate all Rust dependencies, document each one on the website with rationale for inclusion.
      Remove anything not strictly necessary.
  - [ ] Consider a comb-paranoid build that cuts down featureset significantly and dependency chain. depends on how
        painful this would be to do and maintain. For example, dropping all the formatting tools drops the minijinja
        dep. Maybe core dependencies go back to as old a version as makes sense to reduce supply chain attack.
- [ ] `llms.txt` for the website — machine-readable project summary for LLM consumption.

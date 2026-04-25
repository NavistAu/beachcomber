# beachcomber Roadmap

---

### General

- [x] Do we still need to put in support for {% %} style tags in our jinja templates? — already shipped; `{% %}` works in `.f`, `comb e`, and `comb status --format` via `minijinja::Environment::render_str`, and the eval key-discovery scanner (`find_eval_template_pairs` in `src/cli/format.rs`) walks block tags, whitespace-control dashes, and comments. Tests in `tests/template.rs`; examples in `README.md` and `website/docs/reference/cli-commands.md`.
- [ ] Can we / should we consider simdjson for faster json handling?
  - its an extra dep for a very small speed bump, we dont do that much json handling
- [ ] implement a methodology where claude invokes codex
      with a prompt to do a code review and return 0-N highest priority problems/fixes. maybe
      as a skill? loop N times or until codex has no fixes. Same-same but diff prompt for doing
      documentation/website drift?
- [ ] Config-reload semantics for in-flight lifecycle entries — `LifecycleEntry.config` (`src/scheduler/mod.rs:615-619`) is snapshotted at last `on_demand`, so values like `fsevents_reinstate` and `keep_alive_polls` displayed in `comb status` reflect the snapshot, not the live config. No hot-reload signal exists today; if/when one is added (SIGHUP or socket op), lifecycle entries should re-resolve config on next demand. Out of scope per 2026-04-24 brainstorm; raised by the `comb status` TTL design but applies more broadly to any lifecycle-influencing config knob.
- [x] hostname and user seem to update on a 60s cadence? but show --- in ttl. — fixed: `QueryActivity` short-circuits `Once` providers before lifecycle registration (`d6b9fa73`).
- [x] git items, fs event driven, are not showing the fs event icon in ttl — fixed: TTL indicator now keyed on `watches_files` (`bb76d868`), lighter/aligned glyph pair (`dca3dae0`), and mise opts into `fsevents_reinstate=true` by default (`838c3793`) so the ringed variant (`⊙`) is visible out of the box.
- [x] ttl is always 6 chars wide for P, when thats the max pad, otherwise it should right size — fixed: width auto-sizes per snapshot, capped at 6, no lower floor (`b4e922b0` + `dbee4f80`).
- [x] when we have a shell thats in a subdir of a git repo, we get a double up of the same git data. — fixed: `Provider::canonical_path` trait method (`09fa5893`) with overrides for git (`0cb1798d`), mise (`7de4654f`), direnv (`54cf4a85`), asdf (`3c03ce3e`), terraform (`6b09e7a8`), python (`78e0e499`). Each walks up to its project marker; `resolve_path` in `src/server.rs` pipes the result, so the cache key, lifecycle entry, and fs-watch path are all keyed on the canonical project root. Side benefit: the five "opposite-problem" providers (mise/direnv/asdf/terraform/python) now serve data from any subdir rather than only at project root.
- [x] should age col in comb status show 00sxN where N is the current iteration of K? — shipped: age column now renders `{age}×{N}` (e.g. `14s×3`) where N is polls fired in the current lifecycle step (0..=K). Computed from `step_deadline − (K × poll_interval)` at snapshot time; `polls_elapsed` flows through `LifecycleSnapshot` → `CacheRow` → status formatter. ASCII mode uses `x` separator to match TTL column convention.
- [x] `comb status` default sort should be `(path, provider, field)` — shipped `7fde7681`. Groups globals (path `-`) together at the top; path-scoped rows grouped by directory.
- [x] Provide a worked example of the `⊙` (fsevents-reinstate) indicator rendering — shipped `838c3793`. `Provider::fsevents_reinstate_default()` trait method (default false); mise opts into `true`. Global/per-provider config still overrides.

### Website (beachcomber.sh)

- [ ] Site analytics — Umami + Cloudflare Web Analytics. Design and rollout managed in ~/ws/analytics/ project.

### External Provider Backends

- ~~Lua backend via `mlua` crate~~ — won't do; dependency/attack surface cost not justified given script and library backends cover the use cases

- [x] mise backend currently puts a structured dataset into project (path specific) and global top level keys. We should
evaluate if this is correct or if the object should be unpacked upwards so mise produces more top level keys without the
redundant wrapper naming convention. — unpacked: `execute(None)` emits one field per global tool (pathless entry); `execute(Some(p))` emits one field per project-scoped tool (path-scoped entry). `comb get mise "$cwd"` returns project tools; `comb get mise.node "$cwd"` returns a single version string. Fields are no longer wrapped in `global`/`project` objects.
- [x] mise global is showing up in status as a Once --- but its age timer appears to be behaving like the project key? — fixed by separating execution: project execution no longer cross-emits the global entry. Global entry is only populated (with its own lifecycle) when `mise` is queried without a path context.
- [ ] Per-cache-entry absolute-path watches — the current scheduler registers fs watches relative to the cache entry's path. Global (pathless) cache entries receive no watches and rely on fallback polling for invalidation.

  **Specific case:** `mise.global` cannot directly observe `~/.config/mise/config.toml` changes; it gets them within the 30s fallback poll window. Acceptable today, but other plausible providers want the same shape — an HTTP provider watching a local credentials file for rotation, a library provider watching an absolute-path state file, etc.

  **Design questions when picked up:**
  - Extend `InvalidationStrategy::Watch` to accept both relative patterns and absolute paths? Or a new `InvalidationStrategy::WatchAbsolute` variant?
  - Watch registration keyed per-`(provider, cache_path)` rather than per-provider, so each cache entry can have its own distinct watch set.
  - Path expansion for `~` and `$XDG_CONFIG_HOME` at registration time — who owns that?
  - Interaction with `fsevents_reinstate` (cache lifecycle) — orthogonal; watches still persist through decay only if that flag is true.

  **Scope when picked up:** affects `src/scheduler.rs` watch-registration code, `src/provider/mod.rs` `InvalidationStrategy`, and any provider that wants to use the new shape (starting with mise.global). Independent of the decay rebuild — doesn't block that work and isn't blocked by it.

### CLI Ergonomics

- [ ] `comb inspect <path>` (working name) — dump every applicable provider's fields for the given directory in one shot. Globals + path-scoped providers evaluated against the path; single command replaces "ask every provider one at a time". Useful for "what does beachcomber know about this dir right now?" as a diagnostic, onboarding, and integration-authoring tool. Format-aware output (json/text/human). Naming bikeshed: `inspect`, `dump`, `snapshot`, `all`, `info`.
- [x] `comb status` UX redesign for lifecycle visibility — implemented 2026-04-24. See @docs/status_ttl.md and @docs/superpowers/specs/2026-04-24-comb-status-ttl-design.md.
- [ ] `comb status` TTL cell: P × K total budget + countdown-to-next-poll display — visualise total time-to-eviction (`base_P × K`) and seconds-until-next-poll alongside the lifecycle countdown. Cell layout currently too dense; defer until the base TTL column ships and we have real-world watch usage to inform the layout. Out of scope per 2026-04-24 brainstorm.
- [ ] `comb status` failure-state TTL cell content — when a provider is in failure-suppress (`⚠` indicator already shown per the in-scope work), swap TTL cell *content* from lifecycle data (`P × K`) to retry-state data (`next_retry_secs`, `attempt N/M`). Carve-out from the in-scope failure-state ⚠ work. Source: `FailureState` at `src/scheduler/mod.rs:131-169` (`consecutive_failures`, `suppressed_until`).

### Additional Providers

- [ ] `ssh` provider — `keys_loaded` (count of loaded keys), `agent_running` (bool). Global, poll. Useful for prompt indicators before push/deploy operations.
- [ ] `gpg` same as ssh, is the agent running, key count, etc
- [ ] `kerberos` provider — active ticket state. Global, poll.
- [ ] `docker` provider — running container count, current context. Support Docker, OrbStack, and Podman. Global, poll.
  - [ ] this could also be project path specific in the case of docker-compose files perhaps?
- [ ] `brew` provider — count of outdated packages. Global, poll (daily). Extend pattern to other package managers: `apt`, `dnf`/`yum`, `apk`, `pacman`.
- [ ] have claude-scanline make a virtual provider that injects the usage stats in as cached values every time it gets hit.

### Performance Validation

- [ ] Hyperfine benchmarks — before/after comparisons for tmux status bar refresh and p10k prompt render with beachcomber vs native tools. Publish results on the website.
- [ ] Docker test containers — controlled environments for each integration target (oh-my-tmux, p10k, starship, etc.) with both stock and beachcomber-integrated configs. Enables reproducible perf tuning, CI benchmarks, and side-by-side demos for upstream adoption PRs.

### Documentation

- [ ] We added a jinja style formatter, thats used in a number of places. we need to ensure our docs are comprehensive about
  its usage.
- [ ] We should do a llm/agent md page in the website that gives a compact token efficient summary of the tool for llm
purposes. this is different to llm.txt which is more for training datasets and marketing.
  - [ ] Is there any possible utility in making a beachcomber skill/plugin?
- [ ] Fun examples — copy-paste script provider recipes for the website. Show beachcomber as a general-purpose cached data layer, not just a devtools daemon. Practical: local weather, quote of the day, today's Wordle number, countdown to next One Piece episode, ISS overhead pass, Hacker News top story, Spotify current track, Home Assistant entity state (front door lock, thermostat, lights via HA REST API). Weird/fun (not all committed, pick the best):
  - Mercury Retrograde deploy guard — HTTP provider polling an astrology API, git pre-push hook refuses to push to production during retrograde. `mercury_yolo_mode = true` to override.
  - Don't Deploy On Friday — same pattern, simpler astrology. Block or warn on pushes to production branches after Thursday EOD.
  - Physical deploy confidence gauge — combine git.dirty, git.ahead, load, battery, time-since-commit into a 0-100 score. Pipe via `comb watch` to an Arduino servo gauge on your desk. Zones: "YOLO", "Maybe", "Safe", "It's Friday Don't".
  - Vibe check emoji — single emoji in prompt computed from weather, day of week, battery, git state, load. Monday + rain + dirty repo + low battery = 💀. Friday + sunshine + clean = 🏖️. Full decision tree as a flowchart.
  - Office thermostat wars — poll HA thermostat, alert in tmux when someone changes it with who and by how much. Rolling sparkline of temperature battles over the day.
  - Banana scale — compute physical length of terminal scrollback in bananas (lines x font size x approximate character height ÷ 17.78cm). Display as a literal string of 🍌 emoji. Utterly useless, deeply satisfying.
  - Claude token odometer — `wc -l` of Claude Code conversation history from the last 24 hours. Show in tmux/prompt as a rolling count of how much you've been talking to your AI pair programmer today.
  - Magic 8-Ball deploy oracle — script provider that caches a random 8-ball response ("Outlook not so good", "Signs point to yes", etc.) and reshakes at random intervals. Git pre-push hook checks `comb get 8ball.sentiment` and blocks deploys on negative readings. Show the current reading in tmux. The universe has opinions about your release schedule.
- [ ] Timeseries / sparkline guide — document how to use `comb store` to accumulate rolling timeseries data (e.g. service response times, error rates, CPU samples) and render sparkline/histogram status tickers in tmux or prompts. Cover append-to-array patterns, TTL-based expiry, and how to structure the data for consumers that render sparklines (e.g. oh-my-tmux, tmux-sparkline, unicode block characters).

### Project Quality

- [ ] Dependency audit — evaluate all Rust dependencies, document each one on the website with rationale for inclusion. Remove anything not strictly necessary.
  - [ ] Consider a comb-paranoid build that cuts down featureset significantly and dependency chain. depends on how painful
  this would be to do and maintain. For example, dropping all the formatting tools drops the minijinja dep. Maybe core
  dependencies go back to as old a version as makes sense to reduce supply chain attack.
- [ ] `llms.txt` for the website — machine-readable project summary for LLM consumption.
- [ ] Centralise the version number — currently every release touches 13+ files (`Cargo.toml`, `beachcomber-client/Cargo.toml`, 5 SDK manifests, 3 AUR PKGBUILDs, nix flake, README deb/rpm URLs, release.yml rockspec filename, plus the Lua rockspec rename). Options: a single `VERSION` file that a release script templates into each manifest; `cargo xtask release` that reads `Cargo.toml` and rewrites the rest; a release-plz / cargo-release workflow; or a CI-side step that patches non-Rust manifests from the Cargo version before publish. Goal: one edit + one command, no per-file fanout. **Relationship to runtime version:** as of the daemon-singleton work (2026-04-24), `BEACHCOMBER_VERSION` (emitted by `build.rs`) is the canonical runtime build identity — includes git sha for dev/dirty builds. That's distinct from `CARGO_PKG_VERSION` which this centralisation work governs (semantic version for releases). Centralisation should keep `Cargo.toml` consistent with all 13+ manifests; `build.rs` continues to read `CARGO_PKG_VERSION` and append the sha suffix.
- [ ] Script/Automate the release process so its more defined and robust rather than ad-hoc each time.

## Dogfooding

- [x] In some directories/projects, our custom p10k injections that give versions for mise tools on the RHS do not render
correctly in some terminals, pushing a bunch of random looking version information over 2 lines below the prompt.
      ie: 
      ```
      ~/ws/CV                                                                                            0.10.4
      22.22.1 0.10.4
      22.22.1 00:34:55
      ❯
      ```

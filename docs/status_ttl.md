# `comb status` UX Redesign — TTL column

**Status:** implemented 2026-04-24 (target release: next release). Implementation design at `docs/superpowers/specs/2026-04-24-comb-status-ttl-design.md`. Visual design below remains the source of truth for the rendering.

**Source:** brainstorm session 2026-04-23/24, refined and approved 2026-04-24. Authoritative design directive from user: the `comb status` output must carry the full lifecycle story per row, render well under `watch(1)`, and let you watch entries evolve (fresh → stale, Active → Decay → eviction) live.

## Resolved decisions (2026-04-24 brainstorm)

The five "Open micro-decisions" listed at the bottom of this document are resolved by USER NOTES inline. The 2026-04-24 brainstorm resolved the following additional questions; full rationale and table form in the superpowers implementation spec:

- **`◉` cell colouring:** the whole TTL cell shares the lifecycle colour, including the trailing indicator. (Overrides this doc's "Trailing `◉` / `F`: default color" line below.)
- **Indicator capability gate:** `◉` renders only when `fsevents_reinstate=true` AND provider has Watch / WatchAndPoll capability.
- **`CacheRow` discriminator:** new `Option<RowKind>` enum with `Lifecycle { decay, watches_files } | Once | Virtual | Transient`. The previous `decay: Option<u8>` field is removed (superseded; SDKs never exposed it).
- **TSV/CSV/sh/minijinja:** mirror the data model — every `CacheRow` field is its own column/key; the synthesised `TTL` column is human-preset only.
- **Failure indicator IN scope:** `★`→`⚠` swap when provider is in failure-suppress; row turns red. Cell *content* (P × K) unchanged. Backoff/retry data in cell content is roadmap-deferred.
- **CLI:** `--no-color` removed in favour of `--color=auto|always|never`; `--max-width` default bumped 40→120, with new `auto` value (`terminal_size` crate); new `--filter=lifecycle:…`, `--filter=fsevents_reinstate:…`, `--sort=lifecycle`, `--sort=poll_interval`.
- **Tests:** mirror `tests/status_shape.rs`'s hand-maintained `assert!(out.contains(...))` style; do not introduce `insta`.

## Goal

Make `watch -c comb status` a first-class diagnostic view: you can open a pane, run watch, and see providers pulse through their lifecycle without any other tooling.

## Columns

`PROVIDER PATH FIELD VALUE AGE TTL`

Dropped: `STALE` (folded into `AGE` color + absorbed into `TTL` semantics).

## `TTL` column (new — core of the redesign)

Encodes four per-entry lifecycle facts in one fixed-width column:

- current lifecycle position (Active or which Decay step)
- current poll interval `P` (scales with decay: `P × 2^step`)
- keep-alive count `K` (provider-wide, constant across all steps — see `docs/cache-lifecycle.md`)
- whether fs-event reinstatement is armed (`fsevents_reinstate` provider flag)

### Format

```
<lead> <P:right-pad>s×<K:02> <indicator>
```

Fixed column width. Auto-compute the P-padding to the widest P seen on any rendered row; default floor 4 chars (covers up to `9999s`, enough for Decay4 of any P up to ~600s). K always zero-padded to 2 digits.

**Leading char (lifecycle position) — countdown form:**

| Internal state | Lead char | Meaning                              |
|----------------|-----------|--------------------------------------|
| Active         | `★`       | alive                                |
| Decay1         | `3`       | 3 decay transitions before eviction  |
| Decay2         | `2`       | 2 remaining                          |
| Decay3         | `1`       | 1 remaining                          |
| Decay4         | `0`       | 0 remaining — next tick evicts       |

Countdown (not 0–4 forward) because the number then literally reads as "how close to eviction." Directly corresponds to the same countdown readers see for TTLs elsewhere.

**Trailing indicator — watches-files capability, decorated by reinstate:**

| Capability                                      | Unicode | ASCII | Meaning                                                      |
|-------------------------------------------------|---------|-------|--------------------------------------------------------------|
| No fs watches (poll-only)                       | (space) | (space) | Invalidation is purely time-based.                          |
| `Watch` / `WatchAndPoll`, `fsevents_reinstate=false` | `∙` (U+2219) | `-`   | File events invalidate Active entries; no reinstate from decay. |
| `Watch` / `WatchAndPoll`, `fsevents_reinstate=true`  | `⊙` (U+2299) | `+`   | File events also reinstate decayed entries to Active.       |

The glyph progression is visual: dot → dot with a ring around it (decoration = reinstate-armed). Both glyphs are math operator class, so fonts render them at a shared baseline (earlier bullet/circled-bullet pair crossed Unicode blocks and rendered with a visible vertical offset). Reinstate is a filterable fact (`--filter=fsevents_reinstate=true`) when it matters; the at-a-glance TTL cell answers "is this fs-event driven?" first.

**`×` separator:** U+00D7. ASCII fallback: `x`.

**Separator between leading char and body:** single space. No colon.

**Special cases:**

| Entry kind                              | TTL rendering |
|-----------------------------------------|---------------|
| `Once` providers (hostname/user/uname)  | `---`         |
| Virtual / `comb put` entries            | `---`         |

These still show a real `AGE`.

### Sample output

Providers: `git` (P=60s, K=12, reinstate), `battery` (P=30s, K=4, no reinstate), `mise` (P=300s, K=6, reinstate), `hostname` (Once).

```
PROVIDER  PATH       FIELD    VALUE      AGE     TTL
git       /repo      branch   main       14s     ★   60s×12 ◉
git       /repo      dirty    true       14s     ★   60s×12 ◉
battery   -          percent  87         8s      ★   30s×04
mise      /repo      version  2025.4.3   47m     1  480s×06 ◉
hostname  -          short    artemis    3h      ---
```

Reading:
- `★   60s×12 ◉` → active, polls every 60s, K=12, file events reinstate.
- `1  480s×06 ◉` → Decay3 (one step before Decay4), currently polling at 480s (8×P for mise's P=300s — yes, that's a typo-free misalignment: mise P=300s, Decay3 P=300×8=2400s, not 480s. See implementation note below).
- `---` → no lifecycle (Once / virtual).

**Implementation note on the mise example:** the sample row above uses 480 as illustrative; correct arithmetic for mise with P=300, Decay3 is P×2³=2400s. The implementation should compute the displayed P from the actual `PollTimer.interval` in the lifecycle registry — don't re-derive from base P + step.

### Coloring

| Lead char | Color                 |
|-----------|-----------------------|
| `★`       | green                 |
| `3`       | default / dim         |
| `2`       | yellow / amber        |
| `1`       | amber                 |
| `0`       | red                   |

Trailing `◉` / `F`: default color (not a severity signal; informational).
colour affects whole cell contents, is foreground not background.
brightness of colours should be kept consistent, active should be bright, decaying colours should be dim.

## `AGE` column

Unchanged from today's rendering logic, plus color:

- Humanized (`14s`, `2m`, `1h23m`) — keep current `format_age` output.
- Color: green when fresh (`age_ms < expected_interval`), amber when stale.
- No prefix (the earlier F/S proposal is dropped — countdown in TTL carries the "is this fading?" signal; stale color covers "poll overdue").
- Age is shown unconditionally, including for `Once` and virtual entries.

## Watch-friendly default behaviour

Today `comb status` gates the `human` preset on `stdout.is_terminal()`. Under `watch comb s`, stdout is a pipe → falls back to `tsv`, drops color, drops truncation. That must be fixed.

Required changes:

1. **Default preset → `human` unconditionally.** Scripts that want raw data opt in via `-f tsv` / `-f json`. This is a breaking change for anyone piping `comb status` to another tool; we're pre-1.0, document in CHANGELOG.
2. **Color default:** on when stdout is a TTY **or** `WATCH_INTERVAL` env var is set (procps watch exports it). `NO_COLOR` disables unconditionally.
3. **Truncation:** always applied in `human` preset, regardless of TTY.
4. **Stable default sort:** `(path, provider, field)` so rows don't reshuffle between refreshes and globals (path=None) group together at the top of the table.

## Flags (new)

- `--ascii` — swap Unicode glyphs (`★`, `◉`, `×`) for ASCII equivalents (`*`, `F`, `x`). Covers terminals without good Unicode support.
- Keep existing: `--color=auto|always|never`, `--no-trunc`, `--max-width`, `--filter`, `--sort`.

## Open micro-decisions (resolve in spec/impl)

1. **Units for P in TTL.** Three options; design currently picks (1):
   - (1) AGE humanized, P in raw seconds (current sample). Mixed units but each cell readable on its own.
   - (2) Humanize P too (`★    1m×12 ◉`, `1  8m×06 ◉`). Consistent units, but Decay4 P values become multi-unit (`1h20m`) in one cell.
   - (3) Raw seconds everywhere. Most honest, worst to eyeball under watch.

   USER NOTE: 1 is fine for now.
2. **Active-but-stale rendering.** An Active entry *can* be stale (poll overdue due to failures). `★` stays green by the state-based rule above; staleness shows on AGE only. Confirm this is desired, or decide that a stale `★` should render amber.

   USER NOTE: this is for provider failures. we should change the leader icon from star to something to indicate error like
   U+26A0 and perhaps consider changing the whole row to a red fg or somesuch. we dont currently indicate the fallback retry
   information, but perhaps ttl could change to this data instead. either way, it is something to note into the roadmap as a
   future improvement.
3. **Lead char for Active when ASCII fallback is on.** Currently `*`. Confirm (could be `A`).
   USER NOTE: * is fine.
4. **Column width bound for P.** Floor 4 chars; auto-grow per snapshot. Confirm no cap.
   USER NOTE: we should cap it at a reasonable width, like 6 chars for P is 7+ days of seconds, we dont need that much.
5. **Truncation for `---`.** Render left-aligned, padded to TTL column width. Confirm.
   USER NOTE: yes thats fine.

## Scope and surface area

Touches:

- `src/cli/status_format.rs` — column logic, color, new TTL formatter, `--ascii` handling.
- `src/main.rs` — default preset change, `WATCH_INTERVAL` detection, `--ascii` flag.
- `src/cache.rs::CacheRow` — needs `poll_interval_secs`, `keep_alive_polls`, `fsevents_reinstate` fields added (wired from the lifecycle registry via the same path already used for `decay`).
- `src/server.rs::Request::Status` — populate the new CacheRow fields from `LifecycleRegistry::iter()` (same pattern as `get_lifecycle_decay_levels`).

No protocol change — wire format is already JSON with extensible row structure. SDKs don't need to change (they already tolerate unknown fields).

Tests:
- Unit tests in `cli/status_format.rs` for TTL formatting (all 5 lifecycle states, reinstate on/off, Once, virtual, ASCII fallback).
- Integration test in `tests/` that runs `comb status` via the client and asserts the new columns appear.
- Snapshot tests (via `insta` or similar, or hand-maintained expected strings) for the human preset.

## Prior design artefacts

- Verbatim directive (2026-04-23T05:03:47Z, session `cef25c94`): "we should output a Col (or cols if required) in status cli command that gives `[Decay Level 0-4]/[K]/[P]/[indicator if we are watching fsevents / do they reinit]`. decay level 0 is alive. we might need to revisit age/stale to make it clearer whats going on there. ie a col thats AGE with a prefix of F/S and coloured green/orange. all these values kind of interact so its going to take some thought on how to represent them."
- Follow-ups (2026-04-23/24) refined: pack into one `TTL` column; drop F/S prefix on AGE (color-only); countdown numbering; `★` active symbol; `◉` reinstate indicator; fixed-width P/K padding; watch-friendly defaults.
- Related spec: `docs/cache-lifecycle.md` (authoritative for the state machine — the TTL column is its visual dual).

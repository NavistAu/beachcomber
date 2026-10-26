# scanline fixtures

What scanline's pre-phase-6 config drew, recorded so phase 6 can be held to it: `bars.json` for `bar()` byte for byte
(`library::bar::tests::every_recorded_painter_row_is_painted_byte_for_byte`), and `cells.json` for the phase-6 config
cell for cell (`tests/suite/scanline.rs`).

## `bars.json`

- **Source.** The painter is the one in `docs/superpowers/scanline-one-tree.md` section 3.2: a template loop that draws
  a list of segments column by column with `fg | bg | bold`. The four widgets that use it are `context_bar`, `model`,
  `rate_5h` and `rate_7d`.
- **Method.** The validated `conf.d/10-scanline.toml` of the scanline harness
  (`docs/superpowers/scanline-harness/`, its `sandbox/new` copy) ran on a private daemon built from `feat/one-tree`, with
  `COLORTERM=truecolor`, once as written and once with each painter block replaced by `{{ segs | tojson }}`. The payloads
  are the harness states A to E and R (`gen-payload.py`, `gen-status.py`), and the widths its three sets N, W and X.
  Only the two daemon `failback` lines were changed, to the current `failback('30s', 3)` form.
- **Rows.** Each row has the painter's `segs` (labels reduced to their text, colours and bold), the `width` of the row,
  and the `bytes` the painter drew. Rows with the same segments and width appear once.
- **`bar_bytes`.** One row, `A/W/model/22`, has a marker on the first column of a wide glyph. The painter replaced the
  glyph and dropped a column; `bar()` keeps the row its width. `bar_bytes` is the painter's bytes with that column
  restored and one trailing blank removed.

## `10-scanline.toml`, `states.json` and `cells.json`

- **`10-scanline.toml`** is the file scanline generates, rewritten with phase 6 (the phase-6 plan's section 5.4). One
  fix to the plan's text: its git_branch and git_changes lost the Nerd Font branch glyph U+E0A0 that the section 3.2
  file draws; it is restored here, so those two widgets are the section 3.2 text unchanged.
- **`states.json`** is the harness's inputs per state, reduced to what the config reads: the clock (`now`), the put
  (`payload-<S>-new.json` without `now`, which phase 6 drops), the `git` namespace and the linked worktree's `.git` file
  where the state rendered (`refs-<S>.json`; `.worktrees/one-tree` for A, C and R, `.worktrees/clean` for B, D and E),
  `gitstats`, what the old Source answered there, and the status page's `summary` and `incidents` bodies
  (`http/<S>/api/v2/`, only the fields the Sources read). `widths` is the three width sets.
- **`cells.json`** `rows` is what the section 3.2 file drew for every widget at every state and width set, 216 rows
  named `<state>/<set>/<widget>/<width>`, each as runs of `[characters, look]`. A look is the SGR state the characters
  were drawn in: `b`, `d`, `i`, `u`, `r` (bold, dim, italic, underline, invert), `fg=` and `bg=` (`#rrggbb`).
- **Method.** The section 3.2 file (the harness's `sandbox/new` copy but for its local `url()` host) rendered in
  process on `feat/one-tree` at `747d818`, through the same map `Fetch` as the test, with `COLORTERM=truecolor`,
  `TERM=xterm-256color`, `scanline.now` in the put, and its own daemon lines run over the same bodies with `url()`
  answered by them. The 216 renders are byte-identical to the private-daemon renders of scanline-one-tree section
  3.5's validation, and the harness's `oldbench compare` (scanline's own parser and engine, on the pre-one-tree
  `10-scanline.toml`) finds them cell-identical to what scanline drew before one-tree.
- **`pinned`** is what three rows draw instead, on purpose (plan section 5.2). For `B/N/rate_7d/20` and `B/W/rate_7d/26`
  it is the section 3.2 file with the time-left text dropping a zero part (`6d0h` is `6d`), drawn by the same painter;
  that edit, `< 80` for `<= 80` and `width` for `length` in cwd changed no other row. For `A/W/model/22` it is
  `bars.json`'s `bar_bytes`.

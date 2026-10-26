# scanline bar fixtures

`bars.json` is what scanline's pre-phase-6 painter drew, recorded so `bar()` can be held to it byte for byte
(`library::bar::tests::every_recorded_painter_row_is_painted_byte_for_byte`).

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

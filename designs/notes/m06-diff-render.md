# Diff-render (M6)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Diff-render.*

The pure comparison lives in `engine-render/src/diff.rs` (no GPU — unit-testable everywhere); the
CLI decodes the baseline, renders at the baseline's dimensions (no `--width`/`--height`; re-bless to
resize), and reports pass/fail with `diff_pixels`, `max_channel_delta`, and `diff_bounds`. Defaults
are bit-exact; determinism is promised same-machine/same-adapter only, so **baselines are
per-adapter artifacts** and the report carries the adapter name. The diff PNG's three pixel classes
(red violation / yellow within-threshold / faded-gray identical) are pinned formulas — see
`docs/cli-contract.md`. Blessing is `engine screenshot` — no separate bless flag, deliberately. The
report prints on both pass and fail (a documented stdout exception).

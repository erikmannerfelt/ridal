# Example: the Mannerfelt et al. (2026) consensus as derived items

These two files are a worked example of the derived-layer feature (#205),
and the exact layer vocabulary and expressions that the regression test in
`src/interp/derive_regression_tests.rs` uses to reproduce a published
scientific result from committed data.

- `layers.json` — the layer vocabulary, the project default reducer, and one
  exclusivity group.
- `derived.json` — seven named Rhai expressions over those layers.

To try them in a project, copy them into the project's data directory:

```bash
cp layers.json   <project>/ridal_data/layers/layers.json
cp derived.json  <project>/ridal_data/derived/derived.json
```

or `PUT` them to `/api/v1/layers` and `/api/v1/derived` (project-wide items
need the operator role).

## What the expressions mean

| Item | Type | Meaning |
|---|---|---|
| `thickness` | layer | Ice thickness: the 0.49 order statistic of depth over the union of `bed` and `bed_no_temperate`. The `if`/`else` drops a position where more contributors say the bed is not visible than say it is; `>=` keeps a tie, matching the published algorithm. |
| `cts_depth` | layer | Cold-temperate transition depth: the 0.49 order statistic over the union of `bed_no_temperate` and `temperate_ice`. |
| `thickness_user_*` | attribute | Count, quartiles, sample standard deviation and normalised median absolute deviation of the same pool. |

`percentile` is pandas' `quantile(q, interpolation="lower")`: the order
statistic at `floor(q * (n - 1))`, which never interpolates and so always
returns a value a contributor actually picked. It is **not** `median()`.

## Reducers

`default_reducer` is `shallowest`, which keeps the minimum depth per user per
position. For a reflector drawn as a fold, that keeps only the upper limb —
the intended behaviour, since stray picks on multiples and ringing lie below
the true reflector. A layer may override it (`deepest`, `median`, `mean`).
Reducers are applied at evaluation time and never rewrite the stored picks,
so changing one is reversible.

## A note on the exclusivity group

Only `bed` and `bed_no_temperate` are declared mutually exclusive. They are
two answers to the same question ("where is the bed"), so a contributor
should not hold both at one position.

`bed_no_temperate` and `temperate_ice` are deliberately **not** declared
exclusive here. `bed_cold` answers "where is the bed" while `temperate_ice`
answers "where is the CTS", so they are not contradictory answers to one
question, and the published algorithm counts a contributor's cold bed even
when they also drew a temperate line. Declaring them exclusive changes the
reproduction (see `PROGRESS_LOG.md`, P7).

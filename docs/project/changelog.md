# Changelog

What changed in each release. {doc}`upgrading` says what to do about the
changes that affect existing data, scripts or servers.

## 0.7.0

### Breaking changes

- **Sites** ({doc}`../deploy/sites`, #214, #286). `ridal server start` now
  serves a *site*: one server hosting many projects behind one list of
  accounts. It no longer accepts a project directory. Project accounts and
  `ridal project user` are gone; a project's `users.json` now holds only
  memberships. Every project URL moved under `/p/{key}/` and
  `/api/v1/projects/{key}/`, including under `ridal gui`, which serves its
  project as a site of one at `/p/default/`.
- **The default processing profile changed**, and so does the output of
  `ridal process --default` and `ridal.process(default=True)`. It is now
  `remove_empty_traces`, `zero_corr`, `correct_antenna_separation`, `dewow`,
  `background_removal` and `auto_gain`.
- **`zero_corr`** is one step with a `method` and a `scope` (#262). The
  default method is `coppens`; the pre-0.7 behaviour is `zero_corr(legacy)`.
  `zero_corr_max_peak` is retired in favour of
  `zero_corr(max_peak, trace, peak)`.
- **Travel time to depth** accounts for the antenna separation correctly
  (#263). `correct_antenna_separation` takes a `method` (`slant` by default,
  or `legacy`) and a `direct_velocity`.
- **`dewow`** is a per-trace running median or mean (#265). The old `dewow`
  was a coarse background removal. `normalize_horizontal_magnitudes` is
  retired in favour of `dewow`, and `background_removal` is new.
- **`bandpass`** is zero-phase, run forward and backward (#267). Each cutoff
  is now at −6 dB rather than −3 dB.
- **`auto_gain`** measures the gain below the direct wave's ring-down (#268).
- **Python 3.10 wheels are no longer built**; wheels are built for Python
  3.11 to 3.14 (#201).

### Processing

- All processing steps are defined in one typed registry (#85, #260).
  Arguments are checked before processing starts, and a retired step name
  says what replaces it. {doc}`../reference/steps` is generated from it.
- New steps: {step}`multiply` (#222), {step}`background_removal` (#265),
  {step}`adaptive_siglog` (#255), and {step}`remove_tones` and
  {step}`balance_traces` for interference in 800 MHz Malå ProEx data (#287).
- `zero_corr` falls back to `aic` when a record has no noise before the
  direct wave, instead of failing (#269).

### Rendering

- Render profiles can use colormaps, including `seismic` and
  `siglog-seismic` (#251), and `siglog` source preprocessing (#223).
- `siglog` adapts to each radargram's noise floor (#255).
- Python can render the topographically corrected view:
  `ridal.render(topo=...)`, and `render_topo=` on `ridal.process` and
  `ridal.batch_process` (#289).

### GUI and interpretation

- **Derived layers and attributes** (#205–#210, #216): items computed from
  everyone's picks, such as `median(bed)`, written in a small
  {doc}`expression language <../reference/expressions>`. Each derived item is
  evaluated on its own, so a broken one fails alone (#278), and whole
  numbers are accepted in expressions (#270, #277). Derived points have a new
  format (#219), and derived attributes appear in the cursor readout (#281).
- **Exclusivity groups** (#252, #253): layers that may not overlap, enforced
  when picking and saving.
- A trace view (#221), a button to hide the radargram (#233), and a reworked
  picking and violation feedback (#249).
- Contributor counts in the catalog and the layer panel (#279), and groups
  listed alphabetically by name (#284).
- The GUI asks before splitting or joining a line (#235), checks overlap
  from the first vertex of a new line (#290), lets a derived layer's colour
  be changed (#234), and offers range-fill targets again (#243).
- The image download dialog defaults to a width the server accepts (#137).

### Server

- Quick creation of many accounts at once, with invite links or generated
  passwords (#224), now `ridal site account add-bulk`.
- `--cache-memory-mb` is one budget for the whole server rather than per
  radargram (#288).
- Errors caused by something outside Ridal say who can fix them (#250).
- Site and project histories are JSON Lines files (`audit.jsonl`).

### Documentation

- This site (#275).

## 0.6.1

- `ridal.process` and `ridal.batch_process` accept radargram and group
  identity arguments (#218).

## 0.6.0

The first release with the browser GUI, interpretation, accounts and
`ridal render`.
